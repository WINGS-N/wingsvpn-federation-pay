//! Проверки и работа инструкций
//!
//! Порядок один и тот же везде: сперва кто подписал, потом что аккаунт наш и
//! того самого вида, и только потом деньги. Любая пропущенная проверка - это
//! вынесенная казна через чужой PDA нахуй

use solana_program::account_info::{next_account_info, AccountInfo};
use solana_program::clock::Clock;
use solana_program::entrypoint::ProgramResult;
use solana_program::program::{invoke, invoke_signed};
use solana_program::program_error::ProgramError;
use solana_program::pubkey::Pubkey;
use solana_program::sysvar::Sysvar;

use crate::instruction::Instruction;
use crate::merkle;
use crate::state::{
    epoch_len, Config, Epoch, Fee, Stake, Tag, CONFIG_LEN, FEE_LEN, MAX_FEE_BPS, STAKE_LEN,
};
use crate::{CONFIG_SEED, EPOCH_SEED, FEE_SEED, STAKE_SEED, STAKE_VAULT_SEED, TREASURY_SEED};

pub fn process(program_id: &Pubkey, accounts: &[AccountInfo], input: &[u8]) -> ProgramResult {
    match Instruction::unpack(input)? {
        Instruction::InitConfig {
            epoch_cap,
            unstake_cooldown,
        } => init_config(program_id, accounts, epoch_cap, unstake_cooldown),
        Instruction::PublishEpoch {
            number,
            root,
            total,
            leaves,
        } => publish_epoch(program_id, accounts, number, root, total, leaves),
        Instruction::Claim {
            epoch,
            index,
            amount,
            proof,
        } => claim(program_id, accounts, epoch, index, amount, &proof),
        Instruction::Stake { amount } => stake(program_id, accounts, amount),
        Instruction::CreditStake { amount } => credit_stake(program_id, accounts, amount),
        Instruction::ReleaseStake { amount } => release_stake(program_id, accounts, amount),
        Instruction::RequestUnstake { amount } => request_unstake(program_id, accounts, amount),
        Instruction::WithdrawStake => withdraw_stake(program_id, accounts),
        Instruction::Slash { amount } => slash(program_id, accounts, amount),
        Instruction::SetConfig {
            epoch_cap,
            unstake_cooldown,
        } => set_config(program_id, accounts, epoch_cap, unstake_cooldown),
        Instruction::SetFee { bps } => set_fee(program_id, accounts, bps),
        Instruction::Donate { amount } => donate(program_id, accounts, amount),
        Instruction::WithdrawFee { amount } => withdraw_fee(program_id, accounts, amount),
    }
}

/// Аккаунт наш и того вида, что ждали. Обе проверки обязательны: чужой
/// владелец значит подделку, а сходство размеров ничего не гарантирует
fn owned_by_us(account: &AccountInfo, program_id: &Pubkey, tag: Tag) -> Result<(), ProgramError> {
    if account.owner != program_id {
        return Err(ProgramError::IllegalOwner);
    }
    let data = account.try_borrow_data()?;
    Tag::expect(*data.first().ok_or(ProgramError::InvalidAccountData)?, tag)
}

/// PDA считаем сами, а не верим переданному адресу: иначе достаточно подсунуть
/// свой аккаунт с нужным первым байтом
fn expect_pda(
    got: &Pubkey,
    seeds: &[&[u8]],
    program_id: &Pubkey,
    bump: u8,
) -> Result<(), ProgramError> {
    let mut full: Vec<&[u8]> = seeds.to_vec();
    let bump_seed = [bump];
    full.push(&bump_seed);
    let want = Pubkey::create_program_address(&full, program_id)
        .map_err(|_| ProgramError::InvalidSeeds)?;
    if want != *got {
        return Err(ProgramError::InvalidSeeds);
    }
    Ok(())
}

fn init_config(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    epoch_cap: u64,
    unstake_cooldown: i64,
) -> ProgramResult {
    let iter = &mut accounts.iter();
    let authority = next_account_info(iter)?;
    let config = next_account_info(iter)?;
    let mint = next_account_info(iter)?;
    let system = next_account_info(iter)?;

    if !authority.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }
    let (_, bump) = Pubkey::find_program_address(&[CONFIG_SEED], program_id);
    expect_pda(config.key, &[CONFIG_SEED], program_id, bump)?;
    // Аккаунт заводим сами: ключа у PDA нет, и снаружи создать его некому
    if config.owner != program_id {
        crate::create::create_pda(
            authority,
            config,
            system,
            program_id,
            &[CONFIG_SEED],
            bump,
            CONFIG_LEN,
        )?;
    }

    let mut data = config.try_borrow_mut_data()?;
    if data.len() < CONFIG_LEN {
        return Err(ProgramError::AccountDataTooSmall);
    }
    // Второй init затёр бы башку на свою и увёл казну целиком
    if data[0] != 0 {
        return Err(ProgramError::AccountAlreadyInitialized);
    }
    Config {
        authority: *authority.key,
        mint: *mint.key,
        epoch_cap,
        next_epoch: 0,
        unstake_cooldown,
        bump,
    }
    .pack(&mut data)
}

fn publish_epoch(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    number: u64,
    root: [u8; 32],
    total: u64,
    leaves: u32,
) -> ProgramResult {
    let iter = &mut accounts.iter();
    let authority = next_account_info(iter)?;
    let config = next_account_info(iter)?;
    let epoch = next_account_info(iter)?;
    let system = next_account_info(iter)?;

    if !authority.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }
    owned_by_us(config, program_id, Tag::Config)?;
    let mut config_state = Config::unpack(&config.try_borrow_data()?)?;
    if config_state.authority != *authority.key {
        return Err(ProgramError::IllegalOwner);
    }
    // Номер строго по порядку: дырка или повтор означают, что кто-то публикует
    // эпоху задним числом
    if number != config_state.next_epoch {
        return Err(ProgramError::InvalidArgument);
    }
    // Потолок стоит на случай вскрытой башки: без него один корень уносит всё
    if total > config_state.epoch_cap {
        return Err(ProgramError::InvalidArgument);
    }

    let number_seed = number.to_le_bytes();
    let (_, bump) = Pubkey::find_program_address(&[EPOCH_SEED, &number_seed], program_id);
    expect_pda(epoch.key, &[EPOCH_SEED, &number_seed], program_id, bump)?;
    if epoch.owner != program_id {
        crate::create::create_pda(
            authority,
            epoch,
            system,
            program_id,
            &[EPOCH_SEED, &number_seed],
            bump,
            epoch_len(leaves),
        )?;
    }
    let mut data = epoch.try_borrow_mut_data()?;
    if data.len() < epoch_len(leaves) {
        return Err(ProgramError::AccountDataTooSmall);
    }
    if data[0] != 0 {
        return Err(ProgramError::AccountAlreadyInitialized);
    }
    Epoch {
        number,
        root,
        total,
        claimed: 0,
        leaves,
        bump,
    }
    .pack(&mut data)?;

    config_state.next_epoch = number
        .checked_add(1)
        .ok_or(ProgramError::ArithmeticOverflow)?;
    config_state.pack(&mut config.try_borrow_mut_data()?)?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn claim(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    epoch_number: u64,
    index: u32,
    amount: u64,
    proof: &[[u8; 32]],
) -> ProgramResult {
    let iter = &mut accounts.iter();
    // Кошелёк из листа: он НЕ подписывает. Клеймить может кто угодно, потому что
    // деньги всё равно уходят строго его владельцу - иначе донору пришлось бы
    // держать SOL на комиссию и лезть в терминал, а это не выплата, а квест
    let wallet = next_account_info(iter)?;
    let payer = next_account_info(iter)?;
    let config = next_account_info(iter)?;
    let epoch = next_account_info(iter)?;
    // Казна это обычный токен-аккаунт, а PDA - только его владелец: сам PDA
    // токен-аккаунтом быть не может, тот принадлежит токен-программе
    let treasury_token = next_account_info(iter)?;
    let destination = next_account_info(iter)?;
    let token_program = next_account_info(iter)?;
    // Системную программу клейм не зовёт, но место в списке держит: порядок
    // аккаунтов - часть контракта, и выкинешь её нахуй - поедут все индексы
    let _system = next_account_info(iter)?;
    let treasury_authority = next_account_info(iter)?;

    // Подпись нужна только с того, кто платит за транзакцию и заводит отметку
    if !payer.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }
    owned_by_us(config, program_id, Tag::Config)?;
    owned_by_us(epoch, program_id, Tag::Epoch)?;
    let config_state = Config::unpack(&config.try_borrow_data()?)?;
    let mut epoch_state = Epoch::unpack(&epoch.try_borrow_data()?)?;
    if epoch_state.number != epoch_number {
        return Err(ProgramError::InvalidArgument);
    }

    if index >= epoch_state.leaves {
        return Err(ProgramError::InvalidArgument);
    }
    // Отметка о выплате - один бит в самой эпохе. Отдельный аккаунт на каждого
    // донора стоил ренты за КАЖДУЮ выплату и не возвращался: при полусотне
    // доноров это пара SOL в год на ровном месте
    {
        let mut data = epoch.try_borrow_mut_data()?;
        if Epoch::taken(&data, index)? {
            return Err(ProgramError::AccountAlreadyInitialized);
        }
        Epoch::take(&mut data, index)?;
    }

    // Лист несёт эпоху внутри: без неё одна выплата предъявляется в каждой
    // следующей эпохе
    let leaf = merkle::leaf(index, wallet.key, amount, epoch_number);
    if !merkle::verify(&epoch_state.root, leaf, proof) {
        return Err(ProgramError::InvalidArgument);
    }

    // Куда платим, решает не тот, кто прислал транзакцию, а сам лист: счёт
    // обязан принадлежать кошельку из дерева, иначе любой увёл бы чужую выплату
    expect_token(destination, &config_state.mint, wallet.key)?;

    let taken = epoch_state
        .claimed
        .checked_add(amount)
        .ok_or(ProgramError::ArithmeticOverflow)?;
    // Даже с кривым деревом из эпохи не уйдёт больше объявленного
    if taken > epoch_state.total {
        return Err(ProgramError::InsufficientFunds);
    }
    epoch_state.claimed = taken;
    epoch_state.pack(&mut epoch.try_borrow_mut_data()?)?;

    let (_, treasury_bump) = Pubkey::find_program_address(&[TREASURY_SEED], program_id);
    expect_pda(
        treasury_authority.key,
        &[TREASURY_SEED],
        program_id,
        treasury_bump,
    )?;

    let transfer = crate::token::transfer(
        token_program.key,
        treasury_token.key,
        destination.key,
        treasury_authority.key,
        amount,
    );
    invoke_signed(
        &transfer,
        &[
            treasury_token.clone(),
            destination.clone(),
            treasury_authority.clone(),
            token_program.clone(),
        ],
        &[&[TREASURY_SEED, &[treasury_bump]]],
    )?;
    Ok(())
}

fn stake(program_id: &Pubkey, accounts: &[AccountInfo], amount: u64) -> ProgramResult {
    let iter = &mut accounts.iter();
    let owner = next_account_info(iter)?;
    let stake_account = next_account_info(iter)?;
    let source = next_account_info(iter)?;
    let vault = next_account_info(iter)?;
    let token_program = next_account_info(iter)?;
    let system = next_account_info(iter)?;

    if !owner.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }
    let seeds: [&[u8]; 2] = [STAKE_SEED, owner.key.as_ref()];
    let (_, bump) = Pubkey::find_program_address(&seeds, program_id);
    expect_pda(stake_account.key, &seeds, program_id, bump)?;
    // Первый взнос приходит в пустоту: аккаунта ещё нет, а завести PDA снаружи
    // некому. Платит сам донор, ему же и вносить
    if stake_account.owner != program_id {
        crate::create::create_pda(
            owner,
            stake_account,
            system,
            program_id,
            &seeds,
            bump,
            STAKE_LEN,
        )?;
    }

    let mut data = stake_account.try_borrow_mut_data()?;
    if data.len() < STAKE_LEN {
        return Err(ProgramError::AccountDataTooSmall);
    }
    let mut state = if data[0] == 0 {
        Stake {
            owner: *owner.key,
            amount: 0,
            unlock_at: 0,
            pending: 0,
            bump,
        }
    } else {
        let existing = Stake::unpack(&data)?;
        if existing.owner != *owner.key {
            return Err(ProgramError::IllegalOwner);
        }
        existing
    };

    let transfer =
        crate::token::transfer(token_program.key, source.key, vault.key, owner.key, amount);
    solana_program::program::invoke(
        &transfer,
        &[
            source.clone(),
            vault.clone(),
            owner.clone(),
            token_program.clone(),
        ],
    )?;

    state.amount = state
        .amount
        .checked_add(amount)
        .ok_or(ProgramError::ArithmeticOverflow)?;
    state.pack(&mut data)
}

/// Учитывает залог, пришедший на личный счёт донора.
///
/// Сами деньги переносит соседняя инструкция перевода в той же транзакции: её
/// подписывает ключ депозита, потому что личный счёт донора это обычный
/// аккаунт, а не PDA - на адрес программы кошельки и биржи не отправляют
/// вовсе. Значит либо проходит вся транзакция, либо ничего
fn credit_stake(program_id: &Pubkey, accounts: &[AccountInfo], amount: u64) -> ProgramResult {
    let iter = &mut accounts.iter();
    let authority = next_account_info(iter)?;
    let config = next_account_info(iter)?;
    let beneficiary = next_account_info(iter)?;
    let stake_account = next_account_info(iter)?;
    let system = next_account_info(iter)?;

    if !authority.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }
    owned_by_us(config, program_id, Tag::Config)?;
    let config_state = Config::unpack(&config.try_borrow_data()?)?;
    if config_state.authority != *authority.key {
        return Err(ProgramError::IllegalOwner);
    }

    let stake_seeds: [&[u8]; 2] = [STAKE_SEED, beneficiary.key.as_ref()];
    let (_, stake_bump) = Pubkey::find_program_address(&stake_seeds, program_id);
    expect_pda(stake_account.key, &stake_seeds, program_id, stake_bump)?;
    if stake_account.owner != program_id {
        crate::create::create_pda(
            authority,
            stake_account,
            system,
            program_id,
            &stake_seeds,
            stake_bump,
            STAKE_LEN,
        )?;
    }

    let mut data = stake_account.try_borrow_mut_data()?;
    if data.len() < STAKE_LEN {
        return Err(ProgramError::AccountDataTooSmall);
    }
    let mut state = if data[0] == 0 {
        Stake {
            owner: *beneficiary.key,
            amount: 0,
            unlock_at: 0,
            pending: 0,
            bump: stake_bump,
        }
    } else {
        let existing = Stake::unpack(&data)?;
        if existing.owner != *beneficiary.key {
            return Err(ProgramError::IllegalOwner);
        }
        existing
    };

    state.amount = state
        .amount
        .checked_add(amount)
        .ok_or(ProgramError::ArithmeticOverflow)?;
    state.pack(&mut data)
}

/// Вывод залога руками башки: заказ и выдача одной инструкцией.
///
/// Пока вывод не заказан - заказываем и запускаем кулдаун. Заказан и кулдаун
/// вышел - отдаём. Две фазы в одной инструкции стоят десятка строк, а место в
/// аккаунте программы кончается
fn release_stake(program_id: &Pubkey, accounts: &[AccountInfo], amount: u64) -> ProgramResult {
    let iter = &mut accounts.iter();
    let authority = next_account_info(iter)?;
    let config = next_account_info(iter)?;
    let beneficiary = next_account_info(iter)?;
    let stake_account = next_account_info(iter)?;
    let vault = next_account_info(iter)?;
    let destination = next_account_info(iter)?;
    let vault_owner = next_account_info(iter)?;
    let token_program = next_account_info(iter)?;

    if !authority.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }
    owned_by_us(config, program_id, Tag::Config)?;
    owned_by_us(stake_account, program_id, Tag::Stake)?;
    let config_state = Config::unpack(&config.try_borrow_data()?)?;
    if config_state.authority != *authority.key {
        return Err(ProgramError::IllegalOwner);
    }
    let mut state = Stake::unpack(&stake_account.try_borrow_data()?)?;
    if state.owner != *beneficiary.key {
        return Err(ProgramError::IllegalOwner);
    }

    let now = Clock::get()?.unix_timestamp;
    if state.pending == 0 {
        if amount == 0 || amount > state.amount {
            return Err(ProgramError::InsufficientFunds);
        }
        // Кулдаун затем и нужен, чтобы наврать и тут же съебать с залогом было
        // нельзя: расхождение всплывает через эпоху-другую
        state.pending = amount;
        state.unlock_at = now
            .checked_add(config_state.unstake_cooldown)
            .ok_or(ProgramError::ArithmeticOverflow)?;
        return state.pack(&mut stake_account.try_borrow_mut_data()?);
    }
    if now < state.unlock_at {
        return Err(ProgramError::InvalidArgument);
    }

    let (_, vault_bump) = Pubkey::find_program_address(&[STAKE_VAULT_SEED], program_id);
    expect_pda(vault_owner.key, &[STAKE_VAULT_SEED], program_id, vault_bump)?;
    let leaving = state.pending;
    let transfer = crate::token::transfer(
        token_program.key,
        vault.key,
        destination.key,
        vault_owner.key,
        leaving,
    );
    invoke_signed(
        &transfer,
        &[
            vault.clone(),
            destination.clone(),
            vault_owner.clone(),
            token_program.clone(),
        ],
        &[&[STAKE_VAULT_SEED, &[vault_bump]]],
    )?;
    state.amount = state.amount.saturating_sub(leaving);
    state.pending = 0;
    state.unlock_at = 0;
    state.pack(&mut stake_account.try_borrow_mut_data()?)
}

fn request_unstake(program_id: &Pubkey, accounts: &[AccountInfo], amount: u64) -> ProgramResult {
    let iter = &mut accounts.iter();
    let owner = next_account_info(iter)?;
    let config = next_account_info(iter)?;
    let stake_account = next_account_info(iter)?;

    if !owner.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }
    owned_by_us(config, program_id, Tag::Config)?;
    owned_by_us(stake_account, program_id, Tag::Stake)?;
    let config_state = Config::unpack(&config.try_borrow_data()?)?;
    let mut state = Stake::unpack(&stake_account.try_borrow_data()?)?;
    if state.owner != *owner.key {
        return Err(ProgramError::IllegalOwner);
    }
    if amount > state.amount {
        return Err(ProgramError::InsufficientFunds);
    }
    // Кулдаун затем и нужен, чтобы наврать и тут же съебать с залогом было
    // нельзя: расхождение всплывает через эпоху-другую
    let now = Clock::get()?.unix_timestamp;
    state.pending = amount;
    state.unlock_at = now
        .checked_add(config_state.unstake_cooldown)
        .ok_or(ProgramError::ArithmeticOverflow)?;
    state.pack(&mut stake_account.try_borrow_mut_data()?)
}

fn withdraw_stake(program_id: &Pubkey, accounts: &[AccountInfo]) -> ProgramResult {
    let iter = &mut accounts.iter();
    let owner = next_account_info(iter)?;
    let stake_account = next_account_info(iter)?;
    let vault = next_account_info(iter)?;
    let destination = next_account_info(iter)?;
    let vault_owner = next_account_info(iter)?;
    let token_program = next_account_info(iter)?;

    if !owner.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }
    owned_by_us(stake_account, program_id, Tag::Stake)?;
    let mut state = Stake::unpack(&stake_account.try_borrow_data()?)?;
    if state.owner != *owner.key {
        return Err(ProgramError::IllegalOwner);
    }
    if state.pending == 0 {
        return Err(ProgramError::InvalidArgument);
    }
    let now = Clock::get()?.unix_timestamp;
    if now < state.unlock_at {
        return Err(ProgramError::InvalidArgument);
    }

    let (_, vault_bump) = Pubkey::find_program_address(&[STAKE_VAULT_SEED], program_id);
    expect_pda(vault_owner.key, &[STAKE_VAULT_SEED], program_id, vault_bump)?;
    let transfer = crate::token::transfer(
        token_program.key,
        vault.key,
        destination.key,
        vault_owner.key,
        state.pending,
    );
    invoke_signed(
        &transfer,
        &[
            vault.clone(),
            destination.clone(),
            vault_owner.clone(),
            token_program.clone(),
        ],
        &[&[STAKE_VAULT_SEED, &[vault_bump]]],
    )?;

    state.amount = state
        .amount
        .checked_sub(state.pending)
        .ok_or(ProgramError::ArithmeticOverflow)?;
    state.pending = 0;
    state.unlock_at = 0;
    state.pack(&mut stake_account.try_borrow_mut_data()?)
}

fn slash(program_id: &Pubkey, accounts: &[AccountInfo], amount: u64) -> ProgramResult {
    let iter = &mut accounts.iter();
    let authority = next_account_info(iter)?;
    let config = next_account_info(iter)?;
    let stake_account = next_account_info(iter)?;
    let vault = next_account_info(iter)?;
    let treasury = next_account_info(iter)?;
    let vault_owner = next_account_info(iter)?;
    let token_program = next_account_info(iter)?;

    if !authority.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }
    owned_by_us(config, program_id, Tag::Config)?;
    owned_by_us(stake_account, program_id, Tag::Stake)?;
    let config_state = Config::unpack(&config.try_borrow_data()?)?;
    if config_state.authority != *authority.key {
        return Err(ProgramError::IllegalOwner);
    }
    let mut state = Stake::unpack(&stake_account.try_borrow_data()?)?;
    if amount > state.amount {
        return Err(ProgramError::InsufficientFunds);
    }

    let (_, vault_bump) = Pubkey::find_program_address(&[STAKE_VAULT_SEED], program_id);
    expect_pda(vault_owner.key, &[STAKE_VAULT_SEED], program_id, vault_bump)?;
    // Срезанное падает обратно в казну, а не сгорает: этот трафик кто-то всё
    // равно повёз, и деньги остаются в системе
    let transfer = crate::token::transfer(
        token_program.key,
        vault.key,
        treasury.key,
        vault_owner.key,
        amount,
    );
    invoke_signed(
        &transfer,
        &[
            vault.clone(),
            treasury.clone(),
            vault_owner.clone(),
            token_program.clone(),
        ],
        &[&[STAKE_VAULT_SEED, &[vault_bump]]],
    )?;

    state.amount = state
        .amount
        .checked_sub(amount)
        .ok_or(ProgramError::ArithmeticOverflow)?;
    if state.pending > state.amount {
        state.pending = state.amount;
    }
    state.pack(&mut stake_account.try_borrow_mut_data()?)
}

/// Правит конфиг: минт выплат, потолок эпохи и кулдаун.
///
/// Номер следующей эпохи не трогаем: сдвинуть его значит разрешить переписать
/// уже опубликованную эпоху задним числом
fn set_config(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    epoch_cap: u64,
    unstake_cooldown: i64,
) -> ProgramResult {
    let iter = &mut accounts.iter();
    let authority = next_account_info(iter)?;
    let config = next_account_info(iter)?;
    let mint = next_account_info(iter)?;

    if !authority.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }
    owned_by_us(config, program_id, Tag::Config)?;
    let mut state = Config::unpack(&config.try_borrow_data()?)?;
    if state.authority != *authority.key {
        return Err(ProgramError::IllegalOwner);
    }
    state.mint = *mint.key;
    state.epoch_cap = epoch_cap;
    state.unstake_cooldown = unstake_cooldown;
    state.pack(&mut config.try_borrow_mut_data()?)
}
/// Токен-аккаунт того минта и того владельца, что ждали.
///
/// У SPL-аккаунта минт лежит по нулевому смещению, владелец по тридцать второму;
/// без обеих проверок под видом казны подсовывают свой счёт
pub(crate) fn expect_token(
    account: &AccountInfo,
    mint: &Pubkey,
    owner: &Pubkey,
) -> Result<(), ProgramError> {
    let data = account.try_borrow_data()?;
    if data.len() < 72 {
        return Err(ProgramError::InvalidAccountData);
    }
    if data[0..32] != mint.to_bytes() {
        return Err(ProgramError::InvalidAccountData);
    }
    if data[32..64] != owner.to_bytes() {
        return Err(ProgramError::IllegalOwner);
    }
    Ok(())
}

fn set_fee(program_id: &Pubkey, accounts: &[AccountInfo], bps: u16) -> ProgramResult {
    let iter = &mut accounts.iter();
    let authority = next_account_info(iter)?;
    let config = next_account_info(iter)?;
    let fee = next_account_info(iter)?;
    let system = next_account_info(iter)?;

    if !authority.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }
    if bps > MAX_FEE_BPS {
        return Err(ProgramError::InvalidArgument);
    }
    owned_by_us(config, program_id, Tag::Config)?;
    let config_state = Config::unpack(&config.try_borrow_data()?)?;
    if config_state.authority != *authority.key {
        return Err(ProgramError::IllegalOwner);
    }

    let (_, bump) = Pubkey::find_program_address(&[FEE_SEED], program_id);
    expect_pda(fee.key, &[FEE_SEED], program_id, bump)?;
    if fee.owner != program_id {
        crate::create::create_pda(
            authority,
            fee,
            system,
            program_id,
            &[FEE_SEED],
            bump,
            FEE_LEN,
        )?;
    }
    Fee { bps, bump }.pack(&mut fee.try_borrow_mut_data()?)
}

fn donate(program_id: &Pubkey, accounts: &[AccountInfo], amount: u64) -> ProgramResult {
    let iter = &mut accounts.iter();
    let payer = next_account_info(iter)?;
    let config = next_account_info(iter)?;
    let fee = next_account_info(iter)?;
    let from = next_account_info(iter)?;
    let treasury_token = next_account_info(iter)?;
    let fee_token = next_account_info(iter)?;
    let token_program = next_account_info(iter)?;

    // Подписывает владелец счёта-источника: токен-программа сама не пустит
    // никого другого, но проверить дешевле, чем разбирать её ошибку
    if !payer.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }
    if amount == 0 {
        return Err(ProgramError::InvalidArgument);
    }
    owned_by_us(config, program_id, Tag::Config)?;
    owned_by_us(fee, program_id, Tag::Fee)?;
    let config_state = Config::unpack(&config.try_borrow_data()?)?;
    let fee_state = Fee::unpack(&fee.try_borrow_data()?)?;
    expect_pda(fee.key, &[FEE_SEED], program_id, fee_state.bump)?;

    let (treasury_authority, _) = Pubkey::find_program_address(&[TREASURY_SEED], program_id);
    let (fee_authority, _) = Pubkey::find_program_address(&[FEE_SEED], program_id);
    expect_token(treasury_token, &config_state.mint, &treasury_authority)?;
    expect_token(fee_token, &config_state.mint, &fee_authority)?;

    let cut = fee_state.cut(amount)?;
    let rest = amount
        .checked_sub(cut)
        .ok_or(ProgramError::ArithmeticOverflow)?;

    if cut > 0 {
        invoke(
            &crate::token::transfer(token_program.key, from.key, fee_token.key, payer.key, cut),
            &[
                from.clone(),
                fee_token.clone(),
                payer.clone(),
                token_program.clone(),
            ],
        )?;
    }
    if rest > 0 {
        invoke(
            &crate::token::transfer(
                token_program.key,
                from.key,
                treasury_token.key,
                payer.key,
                rest,
            ),
            &[
                from.clone(),
                treasury_token.clone(),
                payer.clone(),
                token_program.clone(),
            ],
        )?;
    }
    Ok(())
}

fn withdraw_fee(program_id: &Pubkey, accounts: &[AccountInfo], amount: u64) -> ProgramResult {
    let iter = &mut accounts.iter();
    let authority = next_account_info(iter)?;
    let config = next_account_info(iter)?;
    let fee = next_account_info(iter)?;
    let fee_token = next_account_info(iter)?;
    let destination = next_account_info(iter)?;
    let token_program = next_account_info(iter)?;

    if !authority.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }
    owned_by_us(config, program_id, Tag::Config)?;
    owned_by_us(fee, program_id, Tag::Fee)?;
    let config_state = Config::unpack(&config.try_borrow_data()?)?;
    if config_state.authority != *authority.key {
        return Err(ProgramError::IllegalOwner);
    }
    let fee_state = Fee::unpack(&fee.try_borrow_data()?)?;
    expect_pda(fee.key, &[FEE_SEED], program_id, fee_state.bump)?;

    let (fee_authority, _) = Pubkey::find_program_address(&[FEE_SEED], program_id);
    expect_token(fee_token, &config_state.mint, &fee_authority)?;

    invoke_signed(
        &crate::token::transfer(
            token_program.key,
            fee_token.key,
            destination.key,
            &fee_authority,
            amount,
        ),
        &[
            fee_token.clone(),
            destination.clone(),
            fee.clone(),
            token_program.clone(),
        ],
        &[&[FEE_SEED, &[fee_state.bump]]],
    )
}

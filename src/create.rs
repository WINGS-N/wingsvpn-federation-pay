//! Создание собственных аккаунтов
//!
//! PDA снаружи не создать: приватного ключа у него нет и подписать создание
//! некому. Значит программа заводит их сама, подписывая семенами, иначе первая
//! же инструкция упирается в пустоту нахуй

use solana_program::account_info::AccountInfo;
use solana_program::entrypoint::ProgramResult;
use solana_program::instruction::{AccountMeta, Instruction};
use solana_program::program::invoke_signed;
use solana_program::pubkey::Pubkey;
use solana_program::rent::Rent;

use solana_program::sysvar::Sysvar;

/// Заводит аккаунт нужного размера и отдаёт его программе.
///
/// Платит тот, кто прислал транзакцию: рента за эпоху копеечная, а вешать её на
/// казну значит подъедать деньги доноров
pub fn create_pda<'a>(
    payer: &AccountInfo<'a>,
    target: &AccountInfo<'a>,
    system: &AccountInfo<'a>,
    program_id: &Pubkey,
    seeds: &[&[u8]],
    bump: u8,
    size: usize,
) -> ProgramResult {
    let lamports = Rent::get()?.minimum_balance(size);
    let mut signer: Vec<&[u8]> = seeds.to_vec();
    let bump_seed = [bump];
    signer.push(&bump_seed);

    invoke_signed(
        &create_account_instruction(payer.key, target.key, lamports, size as u64, program_id),
        &[payer.clone(), target.clone(), system.clone()],
        &[&signer],
    )
}

/// Инструкция System Program, собранная руками.
///
/// Крейт ради неё тянуть незачем: он приволок сериализацию и распух программу на
/// десятки килобайт, а платим мы за размер рентой навсегда. Формат простой -
/// номер инструкции, лампорты, размер и владелец
fn create_account_instruction(
    payer: &Pubkey,
    target: &Pubkey,
    lamports: u64,
    space: u64,
    owner: &Pubkey,
) -> Instruction {
    let mut data = Vec::with_capacity(52);
    data.extend_from_slice(&0u32.to_le_bytes());
    data.extend_from_slice(&lamports.to_le_bytes());
    data.extend_from_slice(&space.to_le_bytes());
    data.extend_from_slice(owner.as_ref());
    Instruction {
        program_id: Pubkey::from([0u8; 32]),
        accounts: vec![
            AccountMeta::new(*payer, true),
            AccountMeta::new(*target, true),
        ],
        data,
    }
}

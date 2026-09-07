//! Состояние программы: конфиг, эпоха, стейк
//!
//! Разбор руками, без Anchor: он тянет сотни килобайт и 2-3 SOL ренты, а тут
//! хватает тридцати. Расплата - владелец, подпись, PDA и дискриминатор
//! проверяются в каждой инструкции вручную, и проебать нельзя ни одну

use solana_program::program_error::ProgramError;
use solana_program::pubkey::Pubkey;

/// Первый байт аккаунта: без него чужой аккаунт того же размера читается как
/// свой, и программа радостно ебошит по чужим данным
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tag {
    Config = 1,
    Epoch = 2,
    Claimed = 3,
    Stake = 4,
    Fee = 5,
}

impl Tag {
    pub fn expect(byte: u8, want: Tag) -> Result<(), ProgramError> {
        if byte != want as u8 {
            return Err(ProgramError::InvalidAccountData);
        }
        Ok(())
    }
}

/// Config - кто платит, чем платит и какие стоят предохранители
#[derive(Clone, Debug)]
pub struct Config {
    /// Башка: только она публикует эпохи и режет стейк
    pub authority: Pubkey,
    /// Мята токена выплат. Лежит параметром: платим USDT, но привязываться к
    /// одному минту навсегда - значит однажды пересобирать программу
    pub mint: Pubkey,
    /// Потолок выплаты за эпоху на случай, когда башку вскрыли: без него один
    /// поддельный корень уносит казну целиком нахуй
    pub epoch_cap: u64,
    /// Сколько эпох уже опубликовано, чтобы номер не переиспользовали
    pub next_epoch: u64,
    /// Кулдаун вывода стейка в секундах
    pub unstake_cooldown: i64,
    pub bump: u8,
}

pub const CONFIG_LEN: usize = 1 + 32 + 32 + 8 + 8 + 8 + 1;

impl Config {
    pub fn unpack(data: &[u8]) -> Result<Self, ProgramError> {
        if data.len() < CONFIG_LEN {
            return Err(ProgramError::InvalidAccountData);
        }
        Tag::expect(data[0], Tag::Config)?;
        Ok(Self {
            authority: Pubkey::new_from_array(array32(&data[1..33])),
            mint: Pubkey::new_from_array(array32(&data[33..65])),
            epoch_cap: u64::from_le_bytes(array8(&data[65..73])),
            next_epoch: u64::from_le_bytes(array8(&data[73..81])),
            unstake_cooldown: i64::from_le_bytes(array8(&data[81..89])),
            bump: data[89],
        })
    }

    pub fn pack(&self, data: &mut [u8]) -> Result<(), ProgramError> {
        if data.len() < CONFIG_LEN {
            return Err(ProgramError::AccountDataTooSmall);
        }
        data[0] = Tag::Config as u8;
        data[1..33].copy_from_slice(self.authority.as_ref());
        data[33..65].copy_from_slice(self.mint.as_ref());
        data[65..73].copy_from_slice(&self.epoch_cap.to_le_bytes());
        data[73..81].copy_from_slice(&self.next_epoch.to_le_bytes());
        data[81..89].copy_from_slice(&self.unstake_cooldown.to_le_bytes());
        data[89] = self.bump;
        Ok(())
    }
}

/// Fee - комиссия оператора: сколько процентов снимается с донатов юзеров.
///
/// Отдельным аккаунтом, а не полем конфига: конфиг уже стоит в цепочке, и
/// растить его значит переносить ренту и ловить старую длину при каждом чтении
#[derive(Clone, Debug)]
pub struct Fee {
    /// Доля в сотых долях процента: 250 это 2.5%
    pub bps: u16,
    pub bump: u8,
}

pub const FEE_LEN: usize = 1 + 2 + 1;

/// Выше этого комиссию не задрать даже своей же рукой: опечатка в одну цифру
/// иначе съедает донат целиком
pub const MAX_FEE_BPS: u16 = 3000;

impl Fee {
    pub fn unpack(data: &[u8]) -> Result<Self, ProgramError> {
        if data.len() < FEE_LEN {
            return Err(ProgramError::InvalidAccountData);
        }
        Tag::expect(data[0], Tag::Fee)?;
        Ok(Self {
            bps: u16::from_le_bytes([data[1], data[2]]),
            bump: data[3],
        })
    }

    pub fn pack(&self, data: &mut [u8]) -> Result<(), ProgramError> {
        if data.len() < FEE_LEN {
            return Err(ProgramError::AccountDataTooSmall);
        }
        data[0] = Tag::Fee as u8;
        data[1..3].copy_from_slice(&self.bps.to_le_bytes());
        data[3] = self.bump;
        Ok(())
    }

    /// Сколько снять с этой суммы.
    ///
    /// Умножение проверяемое: переполнится оно только на сумме порядка
    /// шести квадриллионов, и такой донат честнее отбить, чем досчитать
    pub fn cut(&self, amount: u64) -> Result<u64, ProgramError> {
        let taken = amount
            .checked_mul(self.bps as u64)
            .ok_or(ProgramError::ArithmeticOverflow)?;
        Ok(taken / 10_000)
    }
}

/// Epoch - опубликованный корень раздачи за один период
#[derive(Clone, Debug)]
pub struct Epoch {
    pub number: u64,
    /// Корень дерева. Лист - номер, кошелёк, сумма и номер эпохи
    pub root: [u8; 32],
    /// Сколько всего роздано этой эпохой. Больше него из казны не уйдёт даже
    /// при кривом дереве
    pub total: u64,
    /// Сколько уже забрали
    pub claimed: u64,
    /// Сколько листьев в дереве: под них выделен битмап
    pub leaves: u32,
    pub bump: u8,
}

/// EPOCH_HEAD - всё, кроме битмапа выплаченного
pub const EPOCH_HEAD: usize = 1 + 8 + 32 + 8 + 8 + 4 + 1;

/// epoch_len - сколько места занимает эпоха на столько листьев.
///
/// Битмап живёт прямо тут, а не отдельным аккаунтом на каждую выплату: тот
/// стоил ренты за КАЖДОГО донора в КАЖДОЙ эпохе и не возвращался, а тут байт на
/// восемь человек один раз
pub fn epoch_len(leaves: u32) -> usize {
    EPOCH_HEAD + bitmap_len(leaves)
}

pub fn bitmap_len(leaves: u32) -> usize {
    (leaves as usize).div_ceil(8)
}

impl Epoch {
    pub fn unpack(data: &[u8]) -> Result<Self, ProgramError> {
        if data.len() < EPOCH_HEAD {
            return Err(ProgramError::InvalidAccountData);
        }
        Tag::expect(data[0], Tag::Epoch)?;
        Ok(Self {
            number: u64::from_le_bytes(array8(&data[1..9])),
            root: array32(&data[9..41]),
            total: u64::from_le_bytes(array8(&data[41..49])),
            claimed: u64::from_le_bytes(array8(&data[49..57])),
            leaves: u32::from_le_bytes([data[57], data[58], data[59], data[60]]),
            bump: data[61],
        })
    }

    pub fn pack(&self, data: &mut [u8]) -> Result<(), ProgramError> {
        if data.len() < EPOCH_HEAD {
            return Err(ProgramError::AccountDataTooSmall);
        }
        data[0] = Tag::Epoch as u8;
        data[1..9].copy_from_slice(&self.number.to_le_bytes());
        data[9..41].copy_from_slice(&self.root);
        data[41..49].copy_from_slice(&self.total.to_le_bytes());
        data[49..57].copy_from_slice(&self.claimed.to_le_bytes());
        data[57..61].copy_from_slice(&self.leaves.to_le_bytes());
        data[61] = self.bump;
        Ok(())
    }

    /// taken говорит, забирал ли уже этот лист. Бит на лист: отметка о выплате
    /// стоит один бит вместо целого аккаунта
    pub fn taken(data: &[u8], index: u32) -> Result<bool, ProgramError> {
        let (byte, mask) = bit_of(index);
        let cell = data
            .get(EPOCH_HEAD + byte)
            .ok_or(ProgramError::InvalidAccountData)?;
        Ok(cell & mask != 0)
    }

    /// take ставит бит. Второй заход по тому же листу упрётся в него
    pub fn take(data: &mut [u8], index: u32) -> Result<(), ProgramError> {
        let (byte, mask) = bit_of(index);
        let cell = data
            .get_mut(EPOCH_HEAD + byte)
            .ok_or(ProgramError::InvalidAccountData)?;
        *cell |= mask;
        Ok(())
    }
}

fn bit_of(index: u32) -> (usize, u8) {
    ((index as usize) / 8, 1u8 << ((index % 8) as u8))
}

/// Stake - залог донора, который хочет получать деньги
#[derive(Clone, Debug)]
pub struct Stake {
    pub owner: Pubkey,
    pub amount: u64,
    /// Когда можно забрать. Ноль означает, что вывод не заказан
    pub unlock_at: i64,
    /// Сколько заказано к выводу
    pub pending: u64,
    pub bump: u8,
}

pub const STAKE_LEN: usize = 1 + 32 + 8 + 8 + 8 + 1;

impl Stake {
    pub fn unpack(data: &[u8]) -> Result<Self, ProgramError> {
        if data.len() < STAKE_LEN {
            return Err(ProgramError::InvalidAccountData);
        }
        Tag::expect(data[0], Tag::Stake)?;
        Ok(Self {
            owner: Pubkey::new_from_array(array32(&data[1..33])),
            amount: u64::from_le_bytes(array8(&data[33..41])),
            unlock_at: i64::from_le_bytes(array8(&data[41..49])),
            pending: u64::from_le_bytes(array8(&data[49..57])),
            bump: data[57],
        })
    }

    pub fn pack(&self, data: &mut [u8]) -> Result<(), ProgramError> {
        if data.len() < STAKE_LEN {
            return Err(ProgramError::AccountDataTooSmall);
        }
        data[0] = Tag::Stake as u8;
        data[1..33].copy_from_slice(self.owner.as_ref());
        data[33..41].copy_from_slice(&self.amount.to_le_bytes());
        data[41..49].copy_from_slice(&self.unlock_at.to_le_bytes());
        data[49..57].copy_from_slice(&self.pending.to_le_bytes());
        data[57] = self.bump;
        Ok(())
    }
}

fn array32(src: &[u8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    out.copy_from_slice(src);
    out
}

fn array8(src: &[u8]) -> [u8; 8] {
    let mut out = [0u8; 8];
    out.copy_from_slice(src);
    out
}

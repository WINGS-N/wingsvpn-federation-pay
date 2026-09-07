//! Разбор входных байтов инструкции
//!
//! Формат простой: первый байт - номер, дальше поля в little endian. Без Anchor
//! дискриминатор коротким байтом, зато и места он занимает байт, а не восемь

use solana_program::program_error::ProgramError;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Instruction {
    /// Завести конфиг. Один раз на программу
    InitConfig {
        epoch_cap: u64,
        unstake_cooldown: i64,
    },
    /// Опубликовать корень эпохи. Только башка
    PublishEpoch {
        number: u64,
        root: [u8; 32],
        total: u64,
        /// Сколько листьев в дереве: под них выделяется битмап выплаченного
        leaves: u32,
    },
    /// Забрать своё. Донор платит за эту транзакцию сам
    Claim {
        epoch: u64,
        /// Номер листа в дереве: по нему ставится бит выплаченного
        index: u32,
        amount: u64,
        proof: Vec<[u8; 32]>,
    },
    /// Внести залог
    Stake { amount: u64 },
    /// Заказать вывод залога: дальше кулдаун
    RequestUnstake { amount: u64 },
    /// Забрать залог, когда кулдаун вышел
    WithdrawStake,
    /// Срезать залог. Только башка: срезанное падает обратно в казну
    Slash { amount: u64 },
    /// Поправить конфиг: минт выплат, потолок эпохи, кулдаун. Только башка.
    ///
    /// Без этого единственный способ сменить минт - новая программа по новому
    /// адресу, а это переезд всей казны на ровном месте
    SetConfig {
        epoch_cap: u64,
        unstake_cooldown: i64,
    },
    /// Задать комиссию оператора. Только башка
    SetFee { bps: u16 },
    /// Донат юзера: часть уходит комиссией, остальное в казну.
    ///
    /// Спонсор кладёт деньги прямым переводом токенов и эту инструкцию не
    /// зовёт, поэтому снять с него нечего - удерживать просто негде
    Donate { amount: u64 },
    /// Забрать накопленную комиссию. Только башка
    WithdrawFee { amount: u64 },
    /// Вывести залог донору. Только башка.
    ///
    /// Первый заход заказывает вывод и запускает кулдаун, второй отдаёт деньги
    /// на счёт, который донор назвал для выплат. Своей подписи у донора нет:
    /// он вносил залог обычным переводом и ключа нам не давал
    ReleaseStake { amount: u64 },
    /// Оприходовать залог, пришедший на личный адрес донора. Только башка.
    ///
    /// Обычный Stake требует подписи самого донора, а с биржи никто ничего не
    /// подпишет: там простой перевод токенов. Поэтому деньги приходят на PDA
    /// донора, а эта инструкция переносит их в хранилище залогов
    CreditStake { amount: u64 },
}

impl Instruction {
    pub fn unpack(input: &[u8]) -> Result<Self, ProgramError> {
        let (tag, rest) = input
            .split_first()
            .ok_or(ProgramError::InvalidInstructionData)?;
        Ok(match tag {
            0 => Self::InitConfig {
                epoch_cap: u64::from_le_bytes(take8(rest, 0)?),
                unstake_cooldown: i64::from_le_bytes(take8(rest, 8)?),
            },
            1 => Self::PublishEpoch {
                number: u64::from_le_bytes(take8(rest, 0)?),
                root: take32(rest, 8)?,
                total: u64::from_le_bytes(take8(rest, 40)?),
                leaves: u32::from_le_bytes(take4(rest, 48)?),
            },
            2 => {
                let epoch = u64::from_le_bytes(take8(rest, 0)?);
                let index = u32::from_le_bytes(take4(rest, 8)?);
                let amount = u64::from_le_bytes(take8(rest, 12)?);
                let count = u32::from_le_bytes(take4(rest, 20)?) as usize;
                // Дерево на весь флот выше тридцати уровней не бывает, а без
                // потолка прилетит пруф на мегабайт и сожрёт лимит нахуй
                if count > 32 {
                    return Err(ProgramError::InvalidInstructionData);
                }
                let mut proof = Vec::with_capacity(count);
                for i in 0..count {
                    proof.push(take32(rest, 24 + i * 32)?);
                }
                Self::Claim {
                    epoch,
                    index,
                    amount,
                    proof,
                }
            }
            3 => Self::Stake {
                amount: u64::from_le_bytes(take8(rest, 0)?),
            },
            4 => Self::RequestUnstake {
                amount: u64::from_le_bytes(take8(rest, 0)?),
            },
            5 => Self::WithdrawStake,
            6 => Self::Slash {
                amount: u64::from_le_bytes(take8(rest, 0)?),
            },
            7 => Self::SetConfig {
                epoch_cap: u64::from_le_bytes(take8(rest, 0)?),
                unstake_cooldown: i64::from_le_bytes(take8(rest, 8)?),
            },
            8 => Self::SetFee {
                bps: u16::from_le_bytes(take2(rest, 0)?),
            },
            9 => Self::Donate {
                amount: u64::from_le_bytes(take8(rest, 0)?),
            },
            10 => Self::WithdrawFee {
                amount: u64::from_le_bytes(take8(rest, 0)?),
            },
            11 => Self::CreditStake {
                amount: u64::from_le_bytes(take8(rest, 0)?),
            },
            12 => Self::ReleaseStake {
                amount: u64::from_le_bytes(take8(rest, 0)?),
            },
            _ => return Err(ProgramError::InvalidInstructionData),
        })
    }
}

fn take8(data: &[u8], at: usize) -> Result<[u8; 8], ProgramError> {
    let slice = data
        .get(at..at + 8)
        .ok_or(ProgramError::InvalidInstructionData)?;
    let mut out = [0u8; 8];
    out.copy_from_slice(slice);
    Ok(out)
}

fn take2(data: &[u8], at: usize) -> Result<[u8; 2], ProgramError> {
    let slice = data
        .get(at..at + 2)
        .ok_or(ProgramError::InvalidInstructionData)?;
    let mut out = [0u8; 2];
    out.copy_from_slice(slice);
    Ok(out)
}

fn take4(data: &[u8], at: usize) -> Result<[u8; 4], ProgramError> {
    let slice = data
        .get(at..at + 4)
        .ok_or(ProgramError::InvalidInstructionData)?;
    let mut out = [0u8; 4];
    out.copy_from_slice(slice);
    Ok(out)
}

fn take32(data: &[u8], at: usize) -> Result<[u8; 32], ProgramError> {
    let slice = data
        .get(at..at + 32)
        .ok_or(ProgramError::InvalidInstructionData)?;
    let mut out = [0u8; 32];
    out.copy_from_slice(slice);
    Ok(out)
}

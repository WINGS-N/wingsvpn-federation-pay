//! Перевод SPL-токена, собранный руками
//!
//! Крейт spl-token тянет за собой свой solana-program, и типы Pubkey перестают
//! сходиться с нашими. Инструкция перевода это один байт тега и восемь байт
//! суммы, так что городить ради неё конфликт версий незачем нахуй

use solana_program::instruction::{AccountMeta, Instruction};
use solana_program::pubkey::Pubkey;

/// Номер инструкции Transfer в SPL Token
const TRANSFER: u8 = 3;

/// Перевод с аккаунта на аккаунт. authority подписывает: у казны и хранилища
/// стейка это их же PDA
pub fn transfer(
    token_program: &Pubkey,
    source: &Pubkey,
    destination: &Pubkey,
    authority: &Pubkey,
    amount: u64,
) -> Instruction {
    let mut data = Vec::with_capacity(9);
    data.push(TRANSFER);
    data.extend_from_slice(&amount.to_le_bytes());
    Instruction {
        program_id: *token_program,
        accounts: vec![
            AccountMeta::new(*source, false),
            AccountMeta::new(*destination, false),
            AccountMeta::new_readonly(*authority, true),
        ],
        data,
    }
}

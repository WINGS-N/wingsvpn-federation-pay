//! Деньги федерации WINGS V
//!
//! Программа держит казну, публикует эпохи выплат меркл-корнем и ведёт стейк
//! доноров. Башка тут центробанк: она считает начисления и публикует корень, а
//! цепочка нужна ради аудита и неотзывности - видно, сколько собрано и сколько
//! роздано, и задним числом это не переписать
//!
//! Нативный Rust без Anchor: тот весит сотни килобайт и 2-3 SOL ренты, а тут
//! хватает тридцати и четверти SOL. Цена - каждая проверка написана руками, так
//! что негативные тесты тут не роскошь: только они и стоят между казной и первым
//! же долбоёбом с калькулятором

pub mod create;
pub mod instruction;
pub mod merkle;
pub mod processor;
pub mod state;
pub mod token;

#[cfg(test)]
mod tests;

use solana_program::account_info::AccountInfo;
use solana_program::entrypoint;
use solana_program::entrypoint::ProgramResult;
use solana_program::pubkey::Pubkey;

/// Семена PDA. Казна и стейки живут врозь намеренно: в смешанном виде первая же
/// выплата уедет из чужого залога нахуй
pub const CONFIG_SEED: &[u8] = b"config";
pub const TREASURY_SEED: &[u8] = b"treasury";
pub const EPOCH_SEED: &[u8] = b"epoch";
pub const STAKE_SEED: &[u8] = b"stake";
pub const STAKE_VAULT_SEED: &[u8] = b"stake-vault";
/// Личный адрес донора для залога. Свой на каждого, чтобы приход опознавался по
/// самому адресу: memo при выводе даёт дай бог одна биржа из десяти, и залог
/// без него прилетел бы ничьим
pub const DEPOSIT_SEED: &[u8] = b"deposit";
pub const FEE_SEED: &[u8] = b"fee";

// Дверь для того, кто найдёт дыру: без неё нашедший идёт не к нам, а в твиттер
#[cfg(not(feature = "no-entrypoint"))]
solana_security_txt::security_txt! {
    name: "WINGS V Federation Pay",
    project_url: "https://v.wingsnet.org",
    contacts: "email:security@wingsnet.org",
    policy: "Сообщите о находке письмом до публикации, ответ в течение 72 часов. Тестировать только в devnet",
    preferred_languages: "ru,en",
    auditors: "None"
}

entrypoint!(process_instruction);

pub fn process_instruction(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    input: &[u8],
) -> ProgramResult {
    processor::process(program_id, accounts, input)
}

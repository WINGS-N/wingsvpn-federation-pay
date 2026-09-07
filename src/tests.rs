//! Проверки на то, что программа не отдаёт денег кому попало
//!
//! Разбор инструкций и меркл гоняются на хосте: они чистая арифметика и байты,
//! а живые аккаунты нужны только тем проверкам, которые лезут в цепочку

use crate::instruction::Instruction;
use crate::merkle;
use crate::state::{
    epoch_len, Config, Epoch, Fee, Stake, Tag, CONFIG_LEN, EPOCH_HEAD, FEE_LEN, MAX_FEE_BPS,
    STAKE_LEN,
};
use crate::DEPOSIT_SEED;
use solana_program::pubkey::Pubkey;

fn tree(leaves: &[[u8; 32]]) -> ([u8; 32], Vec<Vec<[u8; 32]>>) {
    // Наивное дерево для проверок: пары складываются по возрастанию, как в
    // программе
    let mut level: Vec<[u8; 32]> = leaves.to_vec();
    let mut proofs: Vec<Vec<[u8; 32]>> = vec![Vec::new(); leaves.len()];
    let mut index: Vec<usize> = (0..leaves.len()).collect();
    while level.len() > 1 {
        let mut next = Vec::new();
        let mut next_index = Vec::new();
        for pair in level.chunks(2) {
            if pair.len() == 2 {
                let (a, b) = (pair[0], pair[1]);
                let node = if a <= b {
                    solana_program::hash::hashv(&[&[0x01u8], &a[..], &b[..]]).to_bytes()
                } else {
                    solana_program::hash::hashv(&[&[0x01u8], &b[..], &a[..]]).to_bytes()
                };
                next.push(node);
            } else {
                next.push(pair[0]);
            }
        }
        for (position, leaf_index) in index.iter().enumerate() {
            let sibling = if position % 2 == 0 {
                position + 1
            } else {
                position - 1
            };
            if sibling < level.len() {
                proofs[*leaf_index].push(level[sibling]);
            }
        }
        for position in 0..next.len() {
            next_index.push(index[position * 2]);
        }
        level = next;
        index = (0..level.len()).collect();
        let _ = next_index;
    }
    (level[0], proofs)
}

#[test]
fn лист_проверяется_своим_пруфом() {
    let a = Pubkey::new_unique();
    let b = Pubkey::new_unique();
    let leaves = [merkle::leaf(0, &a, 100, 7), merkle::leaf(1, &b, 250, 7)];
    let (root, proofs) = tree(&leaves);

    assert!(merkle::verify(&root, leaves[0], &proofs[0]));
    assert!(merkle::verify(&root, leaves[1], &proofs[1]));
}

#[test]
fn чужая_сумма_не_проходит() {
    let a = Pubkey::new_unique();
    let b = Pubkey::new_unique();
    let leaves = [merkle::leaf(0, &a, 100, 7), merkle::leaf(1, &b, 250, 7)];
    let (root, proofs) = tree(&leaves);

    // Та же выплата, но сумма подкручена: дерево про это не знает
    let forged = merkle::leaf(0, &a, 100_000, 7);
    assert!(!merkle::verify(&root, forged, &proofs[0]));
}

#[test]
fn лист_соседней_эпохи_не_проходит() {
    let a = Pubkey::new_unique();
    let b = Pubkey::new_unique();
    let leaves = [merkle::leaf(0, &a, 100, 7), merkle::leaf(1, &b, 250, 7)];
    let (root, proofs) = tree(&leaves);

    // Номер эпохи внутри листа: без него одну выплату предъявляли бы вечно
    let next_epoch = merkle::leaf(0, &a, 100, 8);
    assert!(!merkle::verify(&root, next_epoch, &proofs[0]));
}

#[test]
fn чужой_кошелёк_не_проходит() {
    let a = Pubkey::new_unique();
    let b = Pubkey::new_unique();
    let thief = Pubkey::new_unique();
    let leaves = [merkle::leaf(0, &a, 100, 7), merkle::leaf(1, &b, 250, 7)];
    let (root, proofs) = tree(&leaves);

    assert!(!merkle::verify(
        &root,
        merkle::leaf(0, &thief, 100, 7),
        &proofs[0]
    ));
}

#[test]
fn конфиг_переживает_запись_и_чтение() {
    let mut buf = vec![0u8; CONFIG_LEN];
    let want = Config {
        authority: Pubkey::new_unique(),
        mint: Pubkey::new_unique(),
        epoch_cap: 5_000_000,
        next_epoch: 3,
        unstake_cooldown: 14 * 24 * 3600,
        bump: 255,
    };
    want.pack(&mut buf).unwrap();
    let got = Config::unpack(&buf).unwrap();

    assert_eq!(got.authority, want.authority);
    assert_eq!(got.mint, want.mint);
    assert_eq!(got.epoch_cap, want.epoch_cap);
    assert_eq!(got.next_epoch, want.next_epoch);
    assert_eq!(got.unstake_cooldown, want.unstake_cooldown);
}

#[test]
fn чужой_аккаунт_того_же_размера_не_читается_как_свой() {
    let mut buf = vec![0u8; CONFIG_LEN];
    buf[0] = Tag::Stake as u8;
    assert!(Config::unpack(&buf).is_err());

    let mut epoch = vec![0u8; EPOCH_HEAD];
    epoch[0] = Tag::Config as u8;
    assert!(Epoch::unpack(&epoch).is_err());

    let mut stake = vec![0u8; STAKE_LEN];
    stake[0] = Tag::Epoch as u8;
    assert!(Stake::unpack(&stake).is_err());
}

#[test]
fn инструкции_разбираются() {
    let mut claim = vec![2u8];
    claim.extend_from_slice(&7u64.to_le_bytes());
    claim.extend_from_slice(&0u32.to_le_bytes());
    claim.extend_from_slice(&100u64.to_le_bytes());
    claim.extend_from_slice(&1u32.to_le_bytes());
    claim.extend_from_slice(&[9u8; 32]);

    match Instruction::unpack(&claim).unwrap() {
        Instruction::Claim {
            epoch,
            index,
            amount,
            proof,
        } => {
            assert_eq!(index, 0);
            assert_eq!(epoch, 7);
            assert_eq!(amount, 100);
            assert_eq!(proof, vec![[9u8; 32]]);
        }
        other => panic!("разобралось не в то: {other:?}"),
    }
}

// Тег 11 обязан разбираться отдельно от обычного Stake: путаница между ними
// значит, что залог запишется не тому, кто его прислал
#[test]
fn зачисление_залога_разбирается() {
    let mut raw = vec![11u8];
    raw.extend_from_slice(&5_000_000u64.to_le_bytes());
    match Instruction::unpack(&raw).unwrap() {
        Instruction::CreditStake { amount } => assert_eq!(amount, 5_000_000),
        other => panic!("разобралось не в то: {other:?}"),
    }
}

// Вывод залога отличается от взноса одним байтом тега, и спутать их значит
// вместо возврата денег зачислить их ещё раз
#[test]
fn вывод_залога_разбирается() {
    let mut raw = vec![12u8];
    raw.extend_from_slice(&7_000_000u64.to_le_bytes());
    match Instruction::unpack(&raw).unwrap() {
        Instruction::ReleaseStake { amount } => assert_eq!(amount, 7_000_000),
        other => panic!("разобралось не в то: {other:?}"),
    }
}

// Личный адрес донора выводится только из его кошелька, иначе двое доноров
// уехали бы на один и тот же счёт и залог стал бы общим
#[test]
fn у_каждого_донора_свой_адрес_залога() {
    let program = Pubkey::new_unique();
    let first = Pubkey::new_unique();
    let second = Pubkey::new_unique();
    let (one, _) = Pubkey::find_program_address(&[DEPOSIT_SEED, first.as_ref()], &program);
    let (two, _) = Pubkey::find_program_address(&[DEPOSIT_SEED, second.as_ref()], &program);
    let (again, _) = Pubkey::find_program_address(&[DEPOSIT_SEED, first.as_ref()], &program);
    assert_ne!(one, two);
    assert_eq!(one, again);
}

#[test]
fn пруф_до_небес_отбивается() {
    // Без потолка прилетит пруф на мегабайт и сожрёт лимит инструкции
    let mut claim = vec![2u8];
    claim.extend_from_slice(&7u64.to_le_bytes());
    claim.extend_from_slice(&0u32.to_le_bytes());
    claim.extend_from_slice(&100u64.to_le_bytes());
    claim.extend_from_slice(&10_000u32.to_le_bytes());
    assert!(Instruction::unpack(&claim).is_err());
}

#[test]
fn обрезанная_инструкция_отбивается() {
    assert!(Instruction::unpack(&[]).is_err());
    assert!(Instruction::unpack(&[1u8, 0, 0]).is_err());
    assert!(Instruction::unpack(&[200u8]).is_err());
}

/// Формат листа обязан совпадать с payout.LeafHash в башке байт в байт: она
/// строит корень, а программа его проверяет. Вектор снят с самой башки, и если
/// он разъедется - клеймы перестанут проходить все разом
#[test]
fn формат_листа_сходится_с_башкой() {
    // Адрес 11111111111111111111111111111112 в base58 это 32 нулевых байта с
    // единицей в конце
    let mut raw = [0u8; 32];
    raw[31] = 1;
    let wallet = Pubkey::new_from_array(raw);

    let got = merkle::leaf(0, &wallet, 1_234_567, 7);
    let want = hex_to_32("c3298d015400d14e822dc49540da3f3e1b8a7a98d39ccd09dec78087ae55692d");
    assert_eq!(got, want, "лист разъехался с башкой");
}

fn hex_to_32(text: &str) -> [u8; 32] {
    let mut out = [0u8; 32];
    for (i, chunk) in text.as_bytes().chunks(2).enumerate() {
        let byte = u8::from_str_radix(std::str::from_utf8(chunk).unwrap(), 16).unwrap();
        out[i] = byte;
    }
    out
}

/// Отметка о выплате - бит в самой эпохе: отдельный аккаунт на каждого донора
/// стоил бы ренты за КАЖДУЮ выплату и не возвращался
#[test]
fn бит_выплаты_ставится_и_держится() {
    let leaves = 50u32;
    let mut data = vec![0u8; epoch_len(leaves)];
    Epoch {
        number: 1,
        root: [0u8; 32],
        total: 100,
        claimed: 0,
        leaves,
        bump: 255,
    }
    .pack(&mut data)
    .unwrap();

    assert!(!Epoch::taken(&data, 7).unwrap(), "бит стоял до выплаты");
    Epoch::take(&mut data, 7).unwrap();
    assert!(Epoch::taken(&data, 7).unwrap(), "бит не встал");
    // Соседи не задеты: иначе одна выплата закрывала бы чужие
    assert!(!Epoch::taken(&data, 6).unwrap());
    assert!(!Epoch::taken(&data, 8).unwrap());
    assert!(!Epoch::taken(&data, 49).unwrap());

    // Заголовок пережил запись бита
    let epoch = Epoch::unpack(&data).unwrap();
    assert_eq!(epoch.leaves, leaves);
    assert_eq!(epoch.total, 100);
}

/// Полсотни доноров укладываются в семь байт: ровно ради этого битмап и заводился
#[test]
fn битмап_занимает_копейки() {
    assert_eq!(epoch_len(50) - EPOCH_HEAD, 7);
    assert_eq!(epoch_len(8) - EPOCH_HEAD, 1);
    assert_eq!(epoch_len(0) - EPOCH_HEAD, 0);
}

/// Лист без индекса подставить нельзя: индекс входит в хеш
#[test]
fn индекс_входит_в_лист() {
    let wallet = Pubkey::new_unique();
    assert_ne!(
        merkle::leaf(0, &wallet, 100, 7),
        merkle::leaf(1, &wallet, 100, 7)
    );
}

#[test]
fn комиссия_переживает_запись_и_чтение() {
    let mut buf = vec![0u8; FEE_LEN];
    Fee {
        bps: 250,
        bump: 254,
    }
    .pack(&mut buf)
    .unwrap();
    let got = Fee::unpack(&buf).unwrap();
    assert_eq!(got.bps, 250);
    assert_eq!(got.bump, 254);
}

#[test]
fn комиссия_снимает_свою_долю() {
    let fee = Fee {
        bps: 250,
        bump: 255,
    };
    assert_eq!(fee.cut(10_000_000).unwrap(), 250_000);
    // Мелочь округляется вниз, в пользу того, кто донатит
    assert_eq!(fee.cut(3).unwrap(), 0);
    assert_eq!(fee.cut(0).unwrap(), 0);
}

#[test]
fn комиссия_отбивает_донат_за_гранью_разумного() {
    let fee = Fee {
        bps: MAX_FEE_BPS,
        bump: 255,
    };
    // Столько токенов не существует, но молча посчитать неверно нельзя
    assert!(fee.cut(u64::MAX).is_err());
    // А всё, что бывает в жизни, считается как обещали
    let sane = 1_000_000_000_000_000u64;
    assert_eq!(fee.cut(sane).unwrap(), sane / 10_000 * 3000);
}

#[test]
fn нулевая_комиссия_ничего_не_снимает() {
    let fee = Fee { bps: 0, bump: 255 };
    assert_eq!(fee.cut(1_000_000).unwrap(), 0);
}

#[test]
fn чужой_аккаунт_не_читается_как_комиссия() {
    let mut buf = vec![0u8; FEE_LEN];
    Fee {
        bps: 100,
        bump: 255,
    }
    .pack(&mut buf)
    .unwrap();
    buf[0] = Tag::Config as u8;
    assert!(Fee::unpack(&buf).is_err());
}

#[test]
fn комиссионные_инструкции_разбираются() {
    let mut fee = vec![8u8];
    fee.extend_from_slice(&250u16.to_le_bytes());
    match Instruction::unpack(&fee).unwrap() {
        Instruction::SetFee { bps } => assert_eq!(bps, 250),
        other => panic!("не та инструкция: {other:?}"),
    }

    let mut donate = vec![9u8];
    donate.extend_from_slice(&1_000_000u64.to_le_bytes());
    match Instruction::unpack(&donate).unwrap() {
        Instruction::Donate { amount } => assert_eq!(amount, 1_000_000),
        other => panic!("не та инструкция: {other:?}"),
    }

    let mut withdraw = vec![10u8];
    withdraw.extend_from_slice(&7u64.to_le_bytes());
    match Instruction::unpack(&withdraw).unwrap() {
        Instruction::WithdrawFee { amount } => assert_eq!(amount, 7),
        other => panic!("не та инструкция: {other:?}"),
    }

    // Обрезанная ставка не должна прочитаться нулём
    assert!(Instruction::unpack(&[8u8, 1]).is_err());
}

#[test]
fn счёт_с_чужим_владельцем_или_минтом_не_принимается() {
    use crate::processor::expect_token;
    use solana_program::account_info::AccountInfo;

    let mint = Pubkey::new_unique();
    let owner = Pubkey::new_unique();
    let mut body = vec![0u8; 165];
    body[0..32].copy_from_slice(mint.as_ref());
    body[32..64].copy_from_slice(owner.as_ref());

    let key = Pubkey::new_unique();
    let token_program = Pubkey::new_unique();
    let mut lamports = 1u64;
    let account = AccountInfo::new(
        &key,
        false,
        true,
        &mut lamports,
        &mut body,
        &token_program,
        false,
    );

    assert!(expect_token(&account, &mint, &owner).is_ok());
    // Чужой владелец - это попытка увести деньги на свой счёт
    assert!(expect_token(&account, &mint, &Pubkey::new_unique()).is_err());
    // Чужой минт - выплата не тем, чем обещали
    assert!(expect_token(&account, &Pubkey::new_unique(), &owner).is_err());
}

#[test]
fn обрубок_вместо_токен_счёта_не_принимается() {
    use crate::processor::expect_token;
    use solana_program::account_info::AccountInfo;

    let key = Pubkey::new_unique();
    let token_program = Pubkey::new_unique();
    let mut lamports = 1u64;
    let mut body = vec![0u8; 40];
    let account = AccountInfo::new(
        &key,
        false,
        true,
        &mut lamports,
        &mut body,
        &token_program,
        false,
    );
    assert!(expect_token(&account, &Pubkey::new_unique(), &Pubkey::new_unique()).is_err());
}

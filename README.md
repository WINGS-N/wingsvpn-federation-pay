# wingsvpn-federation-pay

Деньги федерации WINGS V на Solana: казна, эпохи выплат по меркл-корню и стейк
доноров. Башка публикует корень эпохи одной транзакцией, донор клеймит своё сам.

Нативный Rust без Anchor, поэтому каждая проверка написана руками и негативные
тесты обязательны.

## Инструкции

| Номер | Что делает | Кто зовёт |
|---|---|---|
| 0 | `init_config` - завести конфиг | оператор, один раз |
| 1 | `publish_epoch` - опубликовать корень и объём эпохи | башка |
| 2 | `claim` - забрать своё по пруфу | донор |
| 3 | `stake` - внести залог | донор |
| 4 | `request_unstake` - заказать вывод залога | донор |
| 5 | `withdraw_stake` - забрать залог после кулдауна | донор |
| 6 | `slash` - срезать залог в казну | башка |
| 7 | `set_config` - минт, потолок эпохи, кулдаун | башка |
| 8 | `set_fee` - ставка комиссии, потолок 3000 bps | башка |
| 9 | `donate` - донат юзера за вычетом комиссии | кто угодно |
| 10 | `withdraw_fee` - забрать накопленную комиссию | башка |

Спонсорский взнос комиссией не облагается: это обычный перевод токенов на счёт
казны, программу он не зовёт.

## Сборка

```
cargo test
cargo clippy --all-targets

export PATH="$HOME/.local/share/solana/install/active_release/bin:$PATH"
cargo-build-sbf --arch v3
```

Флаг `--arch v3` обязателен, иначе валидатор с SIMD-0178 сборку не примет.

## Деплой

```
solana program deploy target/deploy/wingsvpn_federation_pay.so
```

Апгрейд заводит временный буфер размером с бинарь, так что на кошельке
upgrade authority нужен двойной запас ренты.

## Публичный интерфейс

IDL и security.txt живут в Program Metadata Program, в бинарь не встраиваются:

```
npx @solana-program/program-metadata --rpc <url> -k <keypair> write idl <program-id> idl.json
npx @solana-program/program-metadata --rpc <url> -k <keypair> write security <program-id> security.json
```

Глобальные флаги идут до подкоманды: после неё они игнорируются, и CLI молча
уходит на localhost:8899. Пишет их upgrade authority, вес программы при этом не
растёт.

## Mainnet

Программа `HkehHmkTSBt6nrcwjhvznJfgkEhSd7ysUDVaT2iMqUfT`, выплаты в USDT.

Ключ программы и ключи authority в git не едут: с ними кто угодно перезальёт
программу или уведёт казну.

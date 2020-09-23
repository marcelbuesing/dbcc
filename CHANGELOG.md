# Change Log
## [2.1.2](https://github.com/marcelbuesing/can-dbc/tree/2.1.1) (2020-09-23)
- Fix missing cast to u16 in SFF identifiers.

## [2.1.1](https://github.com/marcelbuesing/can-dbc/tree/2.1.1) (2020-09-23)
- Update tokio-socketcan-bcm to v1.0.0, this removes the need for futures compat.

## [2.1.0](https://github.com/marcelbuesing/can-dbc/tree/2.1.0) (2020-07-22)
- Internally migrate from `codegen`-crate to `quote`- and `proc-macro2`-crate.
- Add `ID` associated const to message struct implementation.

## [2.0.0](https://github.com/marcelbuesing/can-dbc/tree/2.0.0) (2019-04-09)
- Change CAN message id type from `u64` to `u32`.
- Update dependencies

## [1.1.0](https://github.com/marcelbuesing/can-dbc/tree/1.1.0) (2019-01-18)
- Add optional feature `with-serde` and derive Serialize for structs and enums.

## [1.0.1](https://github.com/marcelbuesing/can-dbc/tree/1.0.1) (2019-01-15)

### dbcc
- Add first version of dbc to rust compiler

### can-dbc
- Fix plain attribute definition
- Replace singlespace with multispace seperators (less strict)
- Allow multiple signal groups in DBC document
- Accept signal-less messages
- Accept lists in message transmitters
- Lists may now be empty

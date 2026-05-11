Hash256 GPU miner
==================

Rust/OpenCL miner for Hash256-style Keccak mining. It can mine from explicit
challenge/target hex, from a simple HTTP JSON endpoint, or directly from the
Hash256 Ethereum contract state.

What it does
- Runs an OpenCL Keccak-256 kernel on the GPU.
- Verifies every hit on CPU before reporting it.
- Uses the Hash256-compatible preimage format: `challenge || uint256(nonce)`.
- Limits GPU load with a duty-cycle throttle. The default is 50%.
- Reads on-chain work with Ethereum JSON-RPC via `getChallenge(address)` and
  `miningState()`.
- Prints `mine(uint256)` calldata for any valid hit.

What it does not do
- It does not hold a private key or sign Ethereum transactions. Found nonces are
  printed as calldata and can also be sent to a custom `--submit-url`.

Build

Install Rust and OpenCL drivers for your GPU. On this machine the GNU toolchain
works without Visual Studio Build Tools:

```powershell
cd D:\Hash256\hashminer
rustup toolchain install stable-x86_64-pc-windows-gnu
cargo +stable-x86_64-pc-windows-gnu build --release
```

Mine Hash256 on-chain state

```powershell
.\target\release\hashminer.exe `
  --rpc-url https://eth.llamarpc.com `
  --wallet-address 0x2e18156f6229a479Ed39C7C127dB9d993c7FA34E `
  --duty 70 `
  --batch-power 23
```

`--gpu-limit 50` is an alias for `--duty 50`. The miner measures each GPU batch
runtime and sleeps between batches so the GPU is busy for roughly that
percentage of wall-clock time.

Manual challenge mode

```powershell
.\target\release\hashminer.exe <challenge_hex> <difficulty_hex> --duty 50
```

HTTP work endpoint mode

```powershell
.\target\release\hashminer.exe `
  --fetch-url https://example.com/work `
  --submit-url https://example.com/submit `
  --keep-mining `
  --duty 50
```

The fetch endpoint must return:

```json
{"challenge":"0x...","difficulty":"0x..."}
```

The submit endpoint receives:

```json
{"nonce":123,"hash":"0x...","calldata":"0x..."}
```

Useful options
- `--batch-size 1048576` changes the amount of work per OpenCL launch.
- `--nonce-start <u64>` sets a deterministic starting nonce.
- `--contract <address>` overrides the default Hash256 contract address:
  `0xAC7b5d06fa1e77D08aea40d46cB7C5923A87A0cc`.


use std::fs;
use std::thread::sleep;
use std::time::Duration;
use std::time::Instant;

use clap::Parser;
use hex::FromHex;
use num_bigint::BigUint;
use ocl::{Buffer, ProQue};
use serde::Deserialize;
use tiny_keccak::{Hasher, Keccak};

/// Simple miner options
#[derive(Parser, Debug)]
#[command(version)]
struct Opts {
    /// Challenge hex (0x...); if omitted use --fetch-url
    challenge: Option<String>,
    /// Difficulty hex (0x...)
    difficulty: Option<String>,
    /// Duty percent (1-99)
    #[arg(short, long, visible_alias = "gpu-limit", default_value_t = 50.0)]
    duty: f32,
    /// Ethereum RPC URL to read on-chain challenge & difficulty
    #[arg(long)]
    rpc_url: Option<String>,
    /// Wallet address to get the challenge for (e.g. 0xYourWalletAddress)
    #[arg(long)]
    wallet_address: Option<String>,
    /// Target Contract Address
    #[arg(long, default_value = "0xAC7b5d06fa1e77D08aea40d46cB7C5923A87A0cc")]
    contract: String,
    /// Fetch challenge from URL that returns JSON {"challenge":"0x..","difficulty":"0x.."}
    #[arg(long)]
    fetch_url: Option<String>,
    /// Submit endpoint to POST solutions to as JSON {"nonce":..,"hash":"0x.."}
    #[arg(long)]
    submit_url: Option<String>,
    /// Batch size power of 2 (e.g., 22 means 2^22 = ~4M nonces per batch). Tune for your GPU.
    #[arg(long, default_value_t = 22)]
    batch_power: u32,
}

#[derive(Deserialize)]
struct ChallengeResp {
    challenge: String,
    difficulty: String,
}

fn hex_to_bytes(s: &str) -> Vec<u8> {
    let s = s.strip_prefix("0x").unwrap_or(s);
    Vec::from_hex(s).expect("invalid hex")
}

fn target_to_32(target: &[u8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    if target.len() >= 32 {
        out.copy_from_slice(&target[target.len() - 32..]);
    } else {
        out[32 - target.len()..].copy_from_slice(target);
    }
    out
}

fn nonce_u256_be(nonce: u64) -> [u8; 32] {
    let mut out = [0u8; 32];
    out[24..].copy_from_slice(&nonce.to_be_bytes());
    out
}

fn compare_hash_to_target(hash: &[u8; 32], target: &[u8]) -> bool {
    let h = BigUint::from_bytes_be(hash);
    let t = BigUint::from_bytes_be(target);
    h < t
}

fn keccak256(data: &[u8]) -> [u8; 32] {
    let mut hasher = Keccak::v256();
    let mut out = [0u8; 32];
    hasher.update(data);
    hasher.finalize(&mut out);
    out
}

fn fetch_challenge(url: &str) -> Option<(Vec<u8>, Vec<u8>)> {
    let resp = reqwest::blocking::get(url).ok()?;
    let j: ChallengeResp = resp.json().ok()?;
    Some((hex_to_bytes(&j.challenge), hex_to_bytes(&j.difficulty)))
}

fn mine_calldata(nonce: u64) -> String {
    format!(
        "0x{}{}",
        function_selector("mine(uint256)"),
        hex::encode(nonce_u256_be(nonce))
    )
}

fn post_solution(url: &str, nonce: u64, hash: &str, calldata: &str) -> Result<(), reqwest::Error> {
    let body = serde_json::json!({"nonce": nonce, "hash": hash, "calldata": calldata});
    let _r = reqwest::blocking::Client::new()
        .post(url)
        .json(&body)
        .send()?;
    Ok(())
}

fn eth_call(rpc_url: &str, to: &str, data: &str) -> Result<String, String> {
    match eth_call_with_block(rpc_url, to, data, "latest") {
        Ok(result) => Ok(result),
        Err(latest_err) => eth_call_with_block(rpc_url, to, data, "safe")
            .map_err(|safe_err| format!("{latest_err}; safe fallback also failed: {safe_err}")),
    }
}

fn eth_call_with_block(rpc_url: &str, to: &str, data: &str, block: &str) -> Result<String, String> {
    let payload = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "eth_call",
        "params": [{ "to": to, "data": data }, block],
        "id": 1
    });

    let resp: serde_json::Value = reqwest::blocking::Client::new()
        .post(rpc_url)
        .json(&payload)
        .send()
        .map_err(|e| format!("RPC request failed: {e}"))?
        .json()
        .map_err(|e| format!("RPC returned invalid JSON: {e}"))?;

    if let Some(error) = resp.get("error") {
        return Err(format!("RPC error at {block}: {error}"));
    }

    resp.get("result")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| format!("RPC response missing result: {resp}"))
}

// Function selectors calculated using Keccak-256
// getChallenge(address) -> 0x9fbaddf6 (no wait, let keccak256 compute it)
fn function_selector(sig: &str) -> String {
    let hash = keccak256(sig.as_bytes());
    hex::encode(&hash[0..4])
}

fn fetch_onchain_data(
    rpc_url: &str,
    contract: &str,
    wallet: &str,
) -> Result<(Vec<u8>, Vec<u8>), String> {
    let wallet_clean = wallet.strip_prefix("0x").unwrap_or(wallet);
    if wallet_clean.len() != 40 || !wallet_clean.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err("wallet address must be a 20-byte hex address".to_string());
    }

    // Call getChallenge(address)
    // 0x + selector + 32-byte padded address
    let get_chal_sel = function_selector("getChallenge(address)");
    let mut address_padded = String::from("0").repeat(64 - wallet_clean.len());
    address_padded.push_str(wallet_clean);

    let chal_data = format!("0x{}{}", get_chal_sel, address_padded);

    let chal_result = eth_call(rpc_url, contract, &chal_data)?;
    let mining_state_sel = function_selector("miningState()");
    let ms_data = format!("0x{}", mining_state_sel);
    let ms_result = eth_call(rpc_url, contract, &ms_data)?;

    let c = hex_to_bytes(&chal_result);
    if c.len() != 32 {
        return Err(format!(
            "getChallenge returned {} bytes, expected 32",
            c.len()
        ));
    }

    let d_full = hex_to_bytes(&ms_result);
    if d_full.len() < 96 {
        return Err(format!(
            "miningState returned {} bytes, expected at least 96",
            d_full.len()
        ));
    }
    let d = d_full[64..96].to_vec();

    Ok((c, d))
}

fn main() {
    let opts = Opts::parse();

    // Get challenge & difficulty
    let (challenge, difficulty_bytes) = if let Some(rpc_url) = opts.rpc_url.as_deref() {
        if let Some(wallet) = opts.wallet_address.as_deref() {
            println!("Fetching on-chain data from RPC for wallet {}...", wallet);
            match fetch_onchain_data(rpc_url, &opts.contract, wallet) {
                Ok((c, d)) => {
                    println!("Challenge: 0x{}", hex::encode(&c));
                    println!("Difficulty: 0x{}", hex::encode(&d));
                    (c, d)
                }
                Err(e) => {
                    eprintln!("Failed to read challenge/difficulty from RPC: {e}");
                    return;
                }
            }
        } else {
            eprintln!("--wallet-address is required when using --rpc-url");
            return;
        }
    } else if let Some(fetch_url) = opts.fetch_url.as_deref() {
        match fetch_challenge(fetch_url) {
            Some((c, d)) => (c, d),
            None => {
                eprintln!("Failed to fetch challenge from {}", fetch_url);
                return;
            }
        }
    } else {
        if opts.challenge.is_none() || opts.difficulty.is_none() {
            eprintln!("Provide challenge & difficulty or --fetch-url");
            return;
        }
        (
            hex_to_bytes(opts.challenge.as_ref().unwrap()),
            hex_to_bytes(opts.difficulty.as_ref().unwrap()),
        )
    };

    let duty = (opts.duty.clamp(1.0, 99.0) / 100.0) as f32;
    println!(
        "Challenge len={} bytes, duty={}%%",
        challenge.len(),
        duty * 100.0
    );

    // Read device-side kernel (Keccak full implementation)
    let kernel_src =
        fs::read_to_string("kernels/keccak256_device.cl").expect("failed to read kernel");

    let batch_size: usize = 1 << opts.batch_power; // configurable via CLI now
    println!(
        "Batch size 2^{} = {} nonces per GPU launch",
        opts.batch_power, batch_size
    );

    let pro_que = ProQue::builder()
        .src(&kernel_src)
        .dims(batch_size)
        .build()
        .expect("Failed create ProQue");

    let challenge_buf = Buffer::<u8>::builder()
        .queue(pro_que.queue().clone())
        .flags(ocl::flags::MEM_READ_ONLY)
        .len(challenge.len())
        .copy_host_slice(&challenge)
        .build()
        .unwrap();

    let target_buf = Buffer::<u8>::builder()
        .queue(pro_que.queue().clone())
        .flags(ocl::flags::MEM_READ_ONLY)
        .len(32)
        .copy_host_slice(&{ target_to_32(&difficulty_bytes).to_vec() })
        .build()
        .unwrap();

    let out_nonces = Buffer::<u64>::builder()
        .queue(pro_que.queue().clone())
        .flags(ocl::flags::MEM_WRITE_ONLY)
        .len(batch_size)
        .build()
        .unwrap();

    let out_hashes = Buffer::<u8>::builder()
        .queue(pro_que.queue().clone())
        .flags(ocl::flags::MEM_WRITE_ONLY)
        .len(batch_size * 32)
        .build()
        .unwrap();

    let out_count = Buffer::<u32>::builder()
        .queue(pro_que.queue().clone())
        .flags(ocl::flags::MEM_READ_WRITE)
        .len(1)
        .build()
        .unwrap();

    let kernel = pro_que
        .kernel_builder("keccak_mine")
        .arg_named("challenge", None::<&Buffer<u8>>)
        .arg_named("challenge_len", challenge.len() as u32)
        .arg_named("nonce_base", 0u64)
        .arg_named("target", None::<&Buffer<u8>>)
        .arg_named("out_nonces", None::<&Buffer<u64>>)
        .arg_named("out_hashes", None::<&Buffer<u8>>)
        .arg_named("out_count", None::<&Buffer<u32>>)
        .build()
        .unwrap();

    kernel.set_arg("challenge", &challenge_buf).unwrap();
    kernel.set_arg("target", &target_buf).unwrap();
    kernel.set_arg("out_nonces", &out_nonces).unwrap();
    kernel.set_arg("out_hashes", &out_hashes).unwrap();
    kernel.set_arg("out_count", &out_count).unwrap();

    let mut nonce_base: u64 = 0;
    let mut hashes_done: u64 = 0;
    let mut last_report = Instant::now();

    loop {
        let zero_arr: &[u32] = &[0u32];
        out_count.write(zero_arr).enq().unwrap();
        kernel.set_arg("nonce_base", nonce_base).unwrap();

        let start = Instant::now();
        unsafe {
            kernel.enq().unwrap();
        }
        pro_que.queue().finish().unwrap();
        let elapsed = start.elapsed();

        let mut count_arr = vec![0u32; 1];
        out_count.read(&mut count_arr).enq().unwrap();
        let found = count_arr[0] as usize;

        if found > 0 {
            let mut nonces = vec![0u64; found];
            out_nonces
                .read(&mut nonces)
                .offset(0)
                .len(found)
                .enq()
                .unwrap();

            let mut hashes = vec![0u8; found * 32];
            out_hashes
                .read(&mut hashes)
                .offset(0)
                .len(found * 32)
                .enq()
                .unwrap();

            for i in 0..found {
                let nonce = nonces[i];
                let mut hash = [0u8; 32];
                hash.copy_from_slice(&hashes[i * 32..(i + 1) * 32]);

                // double-check on CPU
                let mut data = Vec::with_capacity(challenge.len() + 32);
                data.extend_from_slice(&challenge);
                data.extend_from_slice(&nonce_u256_be(nonce));
                let h = keccak256(&data);
                let target_32 = target_to_32(&difficulty_bytes);
                if compare_hash_to_target(&h, &target_32) {
                    let hexhash = format!("0x{}", hex::encode(h));
                    let calldata = mine_calldata(nonce);
                    println!("\n\n==========================================");
                    println!("🎉 HIT! FOUND VALID NONCE => {}", nonce);
                    println!("Hash: {}", hexhash);
                    println!("Calldata: {}", calldata);
                    println!("==========================================\n");

                    // Play beep sound on Windows
                    for _ in 0..5 {
                        let _ = std::process::Command::new("powershell")
                            .arg("-c")
                            .arg("[console]::beep(1000, 300)")
                            .status();
                        sleep(Duration::from_millis(100));
                    }

                    if let Some(url) = opts.submit_url.as_deref() {
                        match post_solution(url, nonce, &hexhash, &calldata) {
                            Ok(_) => println!("Submitted to {}", url),
                            Err(e) => eprintln!("Submit failed: {}", e),
                        }
                    }
                    return;
                } else {
                    println!(
                        "False positive nonce={} device_hash=0x{} cpu_hash=0x{}",
                        nonce,
                        hex::encode(hash),
                        hex::encode(h)
                    );
                }
            }
        }

        let sleep_ms = if duty > 0.0 {
            let ratio = (1.0 - duty) / duty;
            (elapsed.as_millis() as f32 * ratio) as u64
        } else {
            0
        };
        if sleep_ms > 0 {
            sleep(Duration::from_millis(sleep_ms));
        }

        hashes_done += batch_size as u64;
        if last_report.elapsed() >= Duration::from_secs(5) {
            let elapsed_secs = last_report.elapsed().as_secs_f64();
            let hashrate = (hashes_done as f64 / 1_000_000.0) / elapsed_secs;
            println!(
                "Mining... {:.2} MH/s (Elapsed {}s) | Last nonce checked: {}",
                hashrate, elapsed_secs as u64, nonce_base
            );
            hashes_done = 0;
            last_report = Instant::now();
        }

        nonce_base = nonce_base.wrapping_add(batch_size as u64);
    }
}

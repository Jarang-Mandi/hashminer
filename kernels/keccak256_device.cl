// keccak256_device.cl
// OpenCL Keccak-256 implementation (reference-style) adapted for on-device mining.
// This kernel computes keccak256(challenge || nonce) for each global id (nonce = nonce_base + gid)
// and compares the 32-byte digest against a 256-bit big-endian target. If digest < target,
// the nonce and digest are written to out arrays and out_count is incremented atomically.

#pragma OPENCL EXTENSION cl_khr_int64_base_atomics : enable

// Rotates for 64-bit
static inline ulong ROTL64(ulong x, uint s) {
    return (x << s) | (x >> (64 - s));
}

// Keccak-f[1600] permutation on state (25 ulongs)
void keccakf(ulong st[25]) {
    const ulong roundc[24] = {
        0x0000000000000001UL,0x0000000000008082UL,0x800000000000808aUL,0x8000000080008000UL,
        0x000000000000808bUL,0x0000000080000001UL,0x8000000080008081UL,0x8000000000008009UL,
        0x000000000000008aUL,0x0000000000000088UL,0x0000000080008009UL,0x000000008000000aUL,
        0x000000008000808bUL,0x800000000000008bUL,0x8000000000008089UL,0x8000000000008003UL,
        0x8000000000008002UL,0x8000000000000080UL,0x000000000000800aUL,0x800000008000000aUL,
        0x8000000080008081UL,0x8000000000008080UL,0x0000000080000001UL,0x8000000080008008UL
    };

    const uint r[25] = {
        0,  1, 62, 28, 27,
        36, 44, 6, 55, 20,
        3, 10, 43, 25, 39,
        41, 45, 15, 21, 8,
        18, 2, 61, 56, 14
    };

    for (uint round = 0; round < 24; ++round) {
        // Theta
        ulong C[5];
        #pragma unroll
        for (int x = 0; x < 5; ++x) {
            C[x] = st[x] ^ st[x+5] ^ st[x+10] ^ st[x+15] ^ st[x+20];
        }
        #pragma unroll
        for (int x = 0; x < 5; ++x) {
            ulong d = C[(x+4)%5] ^ ROTL64(C[(x+1)%5], 1);
            #pragma unroll
            for (int y = 0; y < 25; y += 5) st[y + x] ^= d;
        }

        // Rho and Pi
        ulong B[25];
        #pragma unroll
        for (int i = 0; i < 25; ++i) B[i] = 0UL;
        
        #pragma unroll
        for (int i = 0; i < 25; ++i) {
            uint x = i % 5;
            uint y = i / 5;
            uint shift = r[i];
            B[y + 5 * ((2 * x + 3 * y) % 5)] = ROTL64(st[i], shift);
        }

        // Chi
        #pragma unroll
        for (int y = 0; y < 25; y += 5) {
            #pragma unroll
            for (int x = 0; x < 5; ++x) {
                st[y + x] = B[y + x] ^ ((~B[y + ((x+1)%5)]) & B[y + ((x+2)%5)]);
            }
        }

        // Iota
        st[0] ^= roundc[round];
    }
}

// Absorb bytes (little endian in lanes) for Keccak-256 (rate 1088 bits => 136 bytes block)
// We will implement a direct sponge for small inputs challenge||nonce.

__kernel void keccak_mine(__global const uchar* challenge,
                          const uint challenge_len,
                          const ulong nonce_base,
                          __global const uchar* target, // 32 bytes big-endian
                          __global ulong* out_nonces,
                          __global uchar* out_hashes, // contiguous 32-byte per found
                          __global uint* out_count) {
    size_t gid = get_global_id(0);
    ulong nonce = nonce_base + (ulong)gid;

    // Build message = challenge || uint256 nonce (32-byte big-endian ABI word)
    // We'll absorb into Keccak state (little-endian words)
    ulong st[25];
    for (int i = 0; i < 25; ++i) st[i] = 0UL;

    // rate = 136 bytes; process full block if message <=136
    // We'll construct a single-block message then pad.
    uchar msg[152]; // challenge up to 120 bytes + 32-byte nonce
    uint mlen = 0;
    for (uint i = 0; i < challenge_len; ++i) msg[mlen++] = challenge[i];
    // append nonce as uint256; the high 24 bytes are zero for the u64 search space
    for (uint i = 0; i < 24; ++i) msg[mlen++] = 0;
    msg[mlen+0] = (uchar)((nonce >> 56) & 0xFF);
    msg[mlen+1] = (uchar)((nonce >> 48) & 0xFF);
    msg[mlen+2] = (uchar)((nonce >> 40) & 0xFF);
    msg[mlen+3] = (uchar)((nonce >> 32) & 0xFF);
    msg[mlen+4] = (uchar)((nonce >> 24) & 0xFF);
    msg[mlen+5] = (uchar)((nonce >> 16) & 0xFF);
    msg[mlen+6] = (uchar)((nonce >> 8) & 0xFF);
    msg[mlen+7] = (uchar)((nonce >> 0) & 0xFF);
    mlen += 8;

    // pad: Keccak pad10*1
    uint rate = 136;
    // message fits into one block if mlen <= rate-1
    // XOR message into state lanes (lanes are 8 bytes, little-endian)
    for (uint i = 0; i < mlen; i++) {
        uint lane = i / 8;
        uint off = (i % 8);
        st[lane] ^= ((ulong)msg[i]) << (8 * off);
    }

    // pad
    uint pad_pos = mlen;
    uint lane = pad_pos / 8;
    uint off = pad_pos % 8;
    st[lane] ^= ((ulong)0x01) << (8 * off);
    st[(rate-1)/8] ^= ((ulong)0x80) << (8 * ((rate-1) % 8));

    // permute
    keccakf(st);

    // squeeze 32 bytes (digest)
    uchar digest[32];
    for (uint i = 0; i < 32; ++i) {
        uint lane = i / 8;
        uint off = (i % 8);
        digest[i] = (uchar)((st[lane] >> (8 * off)) & 0xFFUL);
    }

    // Compare standard Keccak digest bytes to target (uint256 big-endian) lexicographically
    int less = 0;
    for (int i = 0; i < 32; ++i) {
        if (digest[i] < target[i]) { less = 1; break; }
        else if (digest[i] > target[i]) { less = 0; break; }
    }

    if (less) {
        uint idx = atomic_inc(out_count);
        out_nonces[idx] = nonce;
        for (int i = 0; i < 32; ++i) out_hashes[(idx*32) + i] = digest[i];
    }
}

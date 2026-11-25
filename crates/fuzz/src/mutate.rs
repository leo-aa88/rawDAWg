//! Deterministic mutators: byte-level (bit flips, interesting integers, insert/delete/
//! duplicate/truncate, splices from other seeds) and JSON-structure-aware (replace numbers
//! with boundary values, swap subtrees, drop keys, change types, grow arrays).

use serde_json::Value;

/// xorshift64* — deterministic and dependency-free.
#[derive(Clone)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed.max(1))
    }
    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    pub fn below(&mut self, n: usize) -> usize {
        if n == 0 { 0 } else { (self.next_u64() % n as u64) as usize }
    }
    pub fn chance(&mut self, num: u64, den: u64) -> bool {
        self.next_u64() % den < num
    }
}

const INTERESTING_U8: [u8; 9] = [0, 1, 0x7F, 0x80, 0xFF, 0x10, 0x20, 0x40, 0xFE];
const INTERESTING_U32: [u32; 10] = [0, 1, 0x7FFF_FFFF, 0x8000_0000, 0xFFFF_FFFF, 0xFFFF, 0x1_0000, 0x7FFF, 0x8000, 255];

/// Apply 1–4 byte-level mutations. `max_len` bounds the output.
pub fn mutate_bytes(rng: &mut Rng, input: &[u8], corpus: &[Vec<u8>], max_len: usize) -> Vec<u8> {
    let mut v = input.to_vec();
    for _ in 0..1 + rng.below(4) {
        match rng.below(10) {
            0 if !v.is_empty() => {
                let i = rng.below(v.len());
                v[i] ^= 1 << rng.below(8);
            }
            1 if !v.is_empty() => {
                let i = rng.below(v.len());
                v[i] = INTERESTING_U8[rng.below(INTERESTING_U8.len())];
            }
            2 if v.len() >= 4 => {
                let i = rng.below(v.len() - 3);
                let x = INTERESTING_U32[rng.below(INTERESTING_U32.len())];
                let bytes = if rng.chance(1, 2) { x.to_le_bytes() } else { x.to_be_bytes() };
                v[i..i + 4].copy_from_slice(&bytes);
            }
            3 => {
                let i = rng.below(v.len() + 1);
                let n = 1 + rng.below(16);
                let ins: Vec<u8> = (0..n).map(|_| rng.next_u64() as u8).collect();
                v.splice(i..i, ins);
            }
            4 if !v.is_empty() => {
                let i = rng.below(v.len());
                let n = (1 + rng.below(32)).min(v.len() - i);
                v.drain(i..i + n);
            }
            5 if !v.is_empty() => {
                // duplicate a chunk somewhere else
                let i = rng.below(v.len());
                let n = (1 + rng.below(64)).min(v.len() - i);
                let chunk = v[i..i + n].to_vec();
                let at = rng.below(v.len() + 1);
                v.splice(at..at, chunk);
            }
            6 if !v.is_empty() => {
                let n = rng.below(v.len());
                v.truncate(n);
            }
            7 if !corpus.is_empty() => {
                // splice: our head + another seed's tail
                let other = &corpus[rng.below(corpus.len())];
                if !other.is_empty() {
                    let cut = rng.below(v.len() + 1);
                    let from = rng.below(other.len());
                    v.truncate(cut);
                    v.extend_from_slice(&other[from..]);
                }
            }
            8 if v.len() >= 2 => {
                // overwrite a range with a copy of another range (length fields, chunk ids)
                let a = rng.below(v.len());
                let b = rng.below(v.len());
                let n = (1 + rng.below(8)).min(v.len() - a.max(b));
                let chunk = v[a..a + n].to_vec();
                v[b..b + n].copy_from_slice(&chunk);
            }
            _ => {
                if !v.is_empty() {
                    let i = rng.below(v.len());
                    v[i] = v[i].wrapping_add(1 + rng.below(4) as u8);
                }
            }
        }
    }
    v.truncate(max_len);
    v
}

fn count_nodes(v: &Value) -> usize {
    1 + match v {
        Value::Array(a) => a.iter().map(count_nodes).sum(),
        Value::Object(o) => o.values().map(count_nodes).sum(),
        _ => 0,
    }
}

fn nth_node<'a>(v: &'a mut Value, n: &mut usize) -> Option<&'a mut Value> {
    if *n == 0 {
        return Some(v);
    }
    *n -= 1;
    match v {
        Value::Array(a) => {
            for c in a.iter_mut() {
                if let Some(x) = nth_node(c, n) {
                    return Some(x);
                }
            }
            None
        }
        Value::Object(o) => {
            for c in o.values_mut() {
                if let Some(x) = nth_node(c, n) {
                    return Some(x);
                }
            }
            None
        }
        _ => None,
    }
}

fn node_at(v: &mut Value, index: usize) -> &mut Value {
    let mut n = index;
    nth_node(v, &mut n).expect("index below node count")
}

fn boundary_number(rng: &mut Rng) -> Value {
    let pick: [Value; 14] = [
        Value::from(0),
        Value::from(-1),
        Value::from(1),
        Value::from(i64::MAX),
        Value::from(i64::MIN),
        Value::from(u64::MAX),
        Value::from(i32::MAX),
        Value::from(u32::MAX),
        Value::from(1e308),
        Value::from(-1e308),
        Value::from(1e-308),
        Value::from(0.5),
        Value::from(65_536),
        Value::from(960 * 1_000_000i64),
    ];
    if rng.chance(1, 4) { Value::from(rng.next_u64() as i64 >> rng.below(60)) } else { pick[rng.below(pick.len())].clone() }
}

/// Apply 1–3 structure-aware mutations to a JSON document.
pub fn mutate_json(rng: &mut Rng, doc: &Value) -> Value {
    let mut v = doc.clone();
    for _ in 0..1 + rng.below(3) {
        let total = count_nodes(&v);
        let target = rng.below(total);
        // pick a donor subtree (cloned before taking the mutable target)
        let donor_idx = rng.below(total);
        let donor = node_at(&mut v.clone(), donor_idx).clone();
        let node = node_at(&mut v, target);
        match rng.below(9) {
            0 => *node = boundary_number(rng),
            1 => {
                *node = match rng.below(5) {
                    0 => Value::String(String::new()),
                    1 => Value::String("x".repeat(1 + rng.below(2048))),
                    2 => Value::String("../../../etc/passwd".into()),
                    3 => Value::String("\u{0}\u{FEFF}\u{10FFFF}é".into()),
                    _ => Value::String("00000000-0000-0000-0000-000000000000".into()),
                }
            }
            2 => *node = [Value::Null, Value::Bool(true), Value::Array(vec![]), Value::Object(Default::default())][rng.below(4)].clone(),
            3 => {
                // swap in another subtree: duplicate ids, cross references, wrong types
                if count_nodes(&donor) < 4096 {
                    *node = donor;
                }
            }
            4 => {
                if let Value::Object(o) = node
                    && !o.is_empty()
                {
                    let k = o.keys().nth(rng.below(o.len())).cloned().unwrap_or_default();
                    o.remove(&k);
                }
            }
            5 => {
                if let Value::Array(a) = node
                    && !a.is_empty()
                {
                    let i = rng.below(a.len());
                    if rng.chance(1, 2) {
                        a.remove(i);
                    } else if a.len() < 4096 {
                        let dup = a[i].clone();
                        for _ in 0..1 + rng.below(8) {
                            a.push(dup.clone());
                        }
                    }
                }
            }
            6 => {
                if let Value::Number(n) = node {
                    let f = n.as_f64().unwrap_or(0.0);
                    let scaled = match rng.below(4) {
                        0 => -f,
                        1 => f * 1e6,
                        2 => f + 1.0,
                        _ => f - 1.0,
                    };
                    if let Some(num) = serde_json::Number::from_f64(scaled) {
                        *node = Value::Number(num);
                    }
                } else {
                    *node = boundary_number(rng);
                }
            }
            7 => {
                if let Value::Object(o) = node {
                    o.insert("fuzz_unknown".into(), donor);
                }
            }
            _ => {
                if let Value::Bool(b) = node {
                    *b = !*b;
                } else if let Value::String(s) = node {
                    // tweak a character (enum tags, ids, hex hashes)
                    let mut chars: Vec<char> = s.chars().collect();
                    if !chars.is_empty() {
                        let i = rng.below(chars.len());
                        chars[i] = ['0', 'f', 'z', '-', '_', 'A'][rng.below(6)];
                        *s = chars.into_iter().collect();
                    }
                }
            }
        }
    }
    v
}

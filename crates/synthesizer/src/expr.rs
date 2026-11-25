//! Bounded expression language for user DSP nodes (DSP.md §6).
//!
//! Grammar: literals, variables, `+ - * /`, unary `-`, comparisons
//! (`< > <= >= == !=` → 1/0), and the functions `min max abs sin cos tanh exp
//! log clamp select`. No loops, recursion, allocation or external access.
//! Source is parsed off-thread into a stack bytecode with fixed limits; the
//! evaluator is allocation-free, never panics, and returns finite values
//! (invalid domains produce a safe value and bump a diagnostic counter).

use serde::{Deserialize, Serialize};

pub const MAX_INSTRUCTIONS: usize = 256;
pub const MAX_STACK: usize = 64;
pub const MAX_SOURCE_BYTES: usize = 4096;
/// Bytecode format version stored with presets.
pub const BYTECODE_VERSION: u32 = 1;
/// Variables available to expressions, by slot index.
pub const VARIABLES: [&str; 12] = ["x", "in1", "in2", "in3", "in4", "ctrl1", "ctrl2", "ctrl3", "ctrl4", "t", "phase", "sr"];
/// Output values are clamped to this magnitude.
pub const OUTPUT_LIMIT: f32 = 8.0;

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum Op {
    Push(f32),
    Load(u8),
    Add,
    Sub,
    Mul,
    Div,
    Neg,
    Lt,
    Gt,
    Le,
    Ge,
    Eq,
    Ne,
    Min,
    Max,
    Abs,
    Sin,
    Cos,
    Tanh,
    Exp,
    Log,
    Clamp,
    /// select(cond, a, b): cond != 0 ? a : b
    Select,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Program {
    pub version: u32,
    pub source: String,
    pub ops: Vec<Op>,
    /// Maximum stack depth the program reaches (≤ MAX_STACK).
    pub max_depth: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ExprError {
    #[error("source too long ({0} bytes, max {MAX_SOURCE_BYTES})")]
    TooLong(usize),
    #[error("unexpected character '{0}' at {1}")]
    UnexpectedChar(char, usize),
    #[error("unexpected end of expression")]
    UnexpectedEnd,
    #[error("unexpected token '{0}' at {1}")]
    UnexpectedToken(String, usize),
    #[error("unknown variable or function '{0}'")]
    Unknown(String),
    #[error("function {0} expects {1} argument(s)")]
    Arity(String, usize),
    #[error("program exceeds {MAX_INSTRUCTIONS} instructions")]
    TooManyInstructions,
    #[error("program exceeds the stack limit of {MAX_STACK}")]
    StackTooDeep,
    #[error("number literal is not finite")]
    BadNumber,
}

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Num(f32),
    Ident(String),
    Op(char),
    Cmp(&'static str),
    LParen,
    RParen,
    Comma,
}

fn tokenize(src: &str) -> Result<Vec<(Tok, usize)>, ExprError> {
    let chars: Vec<char> = src.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        let start = i;
        if c.is_ascii_digit() || (c == '.' && chars.get(i + 1).is_some_and(|d| d.is_ascii_digit())) {
            let mut s = String::new();
            while i < chars.len()
                && (chars[i].is_ascii_digit()
                    || chars[i] == '.'
                    || chars[i] == 'e'
                    || chars[i] == 'E'
                    || ((chars[i] == '-' || chars[i] == '+') && matches!(chars[i - 1], 'e' | 'E')))
            {
                s.push(chars[i]);
                i += 1;
            }
            let v: f32 = s.parse().map_err(|_| ExprError::UnexpectedToken(s.clone(), start))?;
            if !v.is_finite() {
                return Err(ExprError::BadNumber);
            }
            out.push((Tok::Num(v), start));
            continue;
        }
        if c.is_ascii_alphabetic() || c == '_' {
            let mut s = String::new();
            while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '_') {
                s.push(chars[i]);
                i += 1;
            }
            out.push((Tok::Ident(s), start));
            continue;
        }
        let two: String = chars[i..(i + 2).min(chars.len())].iter().collect();
        let cmp = match two.as_str() {
            "<=" => Some("<="),
            ">=" => Some(">="),
            "==" => Some("=="),
            "!=" => Some("!="),
            _ => None,
        };
        if let Some(op) = cmp {
            out.push((Tok::Cmp(op), start));
            i += 2;
            continue;
        }
        match c {
            '+' | '-' | '*' | '/' => out.push((Tok::Op(c), start)),
            '<' => out.push((Tok::Cmp("<"), start)),
            '>' => out.push((Tok::Cmp(">"), start)),
            '(' => out.push((Tok::LParen, start)),
            ')' => out.push((Tok::RParen, start)),
            ',' => out.push((Tok::Comma, start)),
            _ => return Err(ExprError::UnexpectedChar(c, start)),
        }
        i += 1;
    }
    Ok(out)
}

struct Parser {
    toks: Vec<(Tok, usize)>,
    pos: usize,
    ops: Vec<Op>,
}

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos).map(|(t, _)| t)
    }
    fn next(&mut self) -> Option<(Tok, usize)> {
        let t = self.toks.get(self.pos).cloned();
        self.pos += 1;
        t
    }
    fn emit(&mut self, op: Op) -> Result<(), ExprError> {
        if self.ops.len() >= MAX_INSTRUCTIONS {
            return Err(ExprError::TooManyInstructions);
        }
        self.ops.push(op);
        Ok(())
    }
    // comparison := additive (cmp additive)?
    fn comparison(&mut self) -> Result<(), ExprError> {
        self.additive()?;
        while let Some(Tok::Cmp(c)) = self.peek().cloned() {
            self.pos += 1;
            self.additive()?;
            self.emit(match c {
                "<" => Op::Lt,
                ">" => Op::Gt,
                "<=" => Op::Le,
                ">=" => Op::Ge,
                "==" => Op::Eq,
                _ => Op::Ne,
            })?;
        }
        Ok(())
    }
    fn additive(&mut self) -> Result<(), ExprError> {
        self.term()?;
        while let Some(Tok::Op(c)) = self.peek().cloned() {
            if c != '+' && c != '-' {
                break;
            }
            self.pos += 1;
            self.term()?;
            self.emit(if c == '+' { Op::Add } else { Op::Sub })?;
        }
        Ok(())
    }
    fn term(&mut self) -> Result<(), ExprError> {
        self.unary()?;
        while let Some(Tok::Op(c)) = self.peek().cloned() {
            if c != '*' && c != '/' {
                break;
            }
            self.pos += 1;
            self.unary()?;
            self.emit(if c == '*' { Op::Mul } else { Op::Div })?;
        }
        Ok(())
    }
    fn unary(&mut self) -> Result<(), ExprError> {
        if let Some(Tok::Op('-')) = self.peek() {
            self.pos += 1;
            self.unary()?;
            return self.emit(Op::Neg);
        }
        if let Some(Tok::Op('+')) = self.peek() {
            self.pos += 1;
            return self.unary();
        }
        self.primary()
    }
    fn primary(&mut self) -> Result<(), ExprError> {
        let Some((tok, at)) = self.next() else { return Err(ExprError::UnexpectedEnd) };
        match tok {
            Tok::Num(v) => self.emit(Op::Push(v)),
            Tok::LParen => {
                self.comparison()?;
                match self.next() {
                    Some((Tok::RParen, _)) => Ok(()),
                    Some((t, p)) => Err(ExprError::UnexpectedToken(format!("{t:?}"), p)),
                    None => Err(ExprError::UnexpectedEnd),
                }
            }
            Tok::Ident(name) => {
                if let Some(Tok::LParen) = self.peek() {
                    self.pos += 1;
                    let mut argc = 0;
                    if let Some(Tok::RParen) = self.peek() {
                        self.pos += 1;
                    } else {
                        loop {
                            self.comparison()?;
                            argc += 1;
                            match self.next() {
                                Some((Tok::Comma, _)) => continue,
                                Some((Tok::RParen, _)) => break,
                                Some((t, p)) => return Err(ExprError::UnexpectedToken(format!("{t:?}"), p)),
                                None => return Err(ExprError::UnexpectedEnd),
                            }
                        }
                    }
                    let (op, arity) = match name.as_str() {
                        "min" => (Op::Min, 2),
                        "max" => (Op::Max, 2),
                        "abs" => (Op::Abs, 1),
                        "sin" => (Op::Sin, 1),
                        "cos" => (Op::Cos, 1),
                        "tanh" => (Op::Tanh, 1),
                        "exp" => (Op::Exp, 1),
                        "log" => (Op::Log, 1),
                        "clamp" => (Op::Clamp, 3),
                        "select" => (Op::Select, 3),
                        _ => return Err(ExprError::Unknown(name)),
                    };
                    if argc != arity {
                        return Err(ExprError::Arity(name, arity));
                    }
                    self.emit(op)
                } else {
                    match VARIABLES.iter().position(|v| *v == name) {
                        Some(slot) => self.emit(Op::Load(slot as u8)),
                        None => Err(ExprError::Unknown(name)),
                    }
                }
            }
            t => Err(ExprError::UnexpectedToken(format!("{t:?}"), at)),
        }
    }
}

/// Stack effect of an op: (pops, pushes).
fn effect(op: &Op) -> (usize, usize) {
    match op {
        Op::Push(_) | Op::Load(_) => (0, 1),
        Op::Neg | Op::Abs | Op::Sin | Op::Cos | Op::Tanh | Op::Exp | Op::Log => (1, 1),
        Op::Clamp | Op::Select => (3, 1),
        _ => (2, 1),
    }
}

/// Parse and compile. Non-RT.
pub fn compile(source: &str) -> Result<Program, ExprError> {
    if source.len() > MAX_SOURCE_BYTES {
        return Err(ExprError::TooLong(source.len()));
    }
    let toks = tokenize(source)?;
    if toks.is_empty() {
        return Err(ExprError::UnexpectedEnd);
    }
    let mut p = Parser { toks, pos: 0, ops: Vec::new() };
    p.comparison()?;
    if let Some((t, at)) = p.next() {
        return Err(ExprError::UnexpectedToken(format!("{t:?}"), at));
    }
    // verify stack discipline and depth
    let mut depth = 0usize;
    let mut max_depth = 0usize;
    for op in &p.ops {
        let (pops, pushes) = effect(op);
        if depth < pops {
            return Err(ExprError::UnexpectedEnd);
        }
        depth = depth - pops + pushes;
        max_depth = max_depth.max(depth);
        if max_depth > MAX_STACK {
            return Err(ExprError::StackTooDeep);
        }
    }
    if depth != 1 {
        return Err(ExprError::UnexpectedEnd);
    }
    Ok(Program { version: BYTECODE_VERSION, source: source.to_owned(), ops: p.ops, max_depth })
}

/// Diagnostics counted during evaluation (read off-thread).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ExprDiagnostics {
    pub division_by_zero: u32,
    pub domain_errors: u32,
    pub nonfinite: u32,
}

/// Allocation-free evaluator with a fixed stack.
#[derive(Clone, Debug)]
pub struct Evaluator {
    stack: [f32; MAX_STACK],
    pub diagnostics: ExprDiagnostics,
}

impl Default for Evaluator {
    fn default() -> Self {
        Self::new()
    }
}

impl Evaluator {
    pub fn new() -> Self {
        Self { stack: [0.0; MAX_STACK], diagnostics: ExprDiagnostics::default() }
    }

    /// Evaluate `prog` with variable values `vars` (indexed like [`VARIABLES`]).
    /// Always returns a finite value in ±[`OUTPUT_LIMIT`].
    #[inline]
    pub fn eval(&mut self, prog: &Program, vars: &[f32; VARIABLES.len()]) -> f32 {
        let mut sp = 0usize;
        let st = &mut self.stack;
        for op in &prog.ops {
            match *op {
                Op::Push(v) => {
                    st[sp] = v;
                    sp += 1;
                }
                Op::Load(i) => {
                    st[sp] = vars[usize::from(i) % VARIABLES.len()];
                    sp += 1;
                }
                Op::Neg => st[sp - 1] = -st[sp - 1],
                Op::Abs => st[sp - 1] = st[sp - 1].abs(),
                Op::Sin => st[sp - 1] = st[sp - 1].clamp(-1e6, 1e6).sin(),
                Op::Cos => st[sp - 1] = st[sp - 1].clamp(-1e6, 1e6).cos(),
                Op::Tanh => st[sp - 1] = rawdawg_dsp::math::fast_tanh(st[sp - 1]),
                Op::Exp => st[sp - 1] = st[sp - 1].clamp(-80.0, 20.0).exp(),
                Op::Log => {
                    let x = st[sp - 1];
                    st[sp - 1] = if x > 0.0 {
                        x.ln()
                    } else {
                        self.diagnostics.domain_errors = self.diagnostics.domain_errors.wrapping_add(1);
                        -16.6 // ln(6e-8), a finite floor
                    };
                }
                Op::Add | Op::Sub | Op::Mul | Op::Div | Op::Lt | Op::Gt | Op::Le | Op::Ge | Op::Eq | Op::Ne | Op::Min | Op::Max => {
                    let b = st[sp - 1];
                    let a = st[sp - 2];
                    sp -= 1;
                    st[sp - 1] = match *op {
                        Op::Add => a + b,
                        Op::Sub => a - b,
                        Op::Mul => a * b,
                        Op::Div => {
                            if b.abs() < 1e-12 {
                                self.diagnostics.division_by_zero = self.diagnostics.division_by_zero.wrapping_add(1);
                                0.0
                            } else {
                                a / b
                            }
                        }
                        Op::Lt => f32::from(u8::from(a < b)),
                        Op::Gt => f32::from(u8::from(a > b)),
                        Op::Le => f32::from(u8::from(a <= b)),
                        Op::Ge => f32::from(u8::from(a >= b)),
                        Op::Eq => f32::from(u8::from(a == b)),
                        Op::Ne => f32::from(u8::from(a != b)),
                        Op::Min => a.min(b),
                        _ => a.max(b),
                    };
                }
                Op::Clamp => {
                    let hi = st[sp - 1];
                    let lo = st[sp - 2];
                    let x = st[sp - 3];
                    sp -= 2;
                    st[sp - 1] = if lo <= hi { x.clamp(lo, hi) } else { x.clamp(hi, lo) };
                }
                Op::Select => {
                    let b = st[sp - 1];
                    let a = st[sp - 2];
                    let c = st[sp - 3];
                    sp -= 2;
                    st[sp - 1] = if c != 0.0 { a } else { b };
                }
            }
        }
        let out = st[sp.saturating_sub(1)];
        if out.is_finite() {
            out.clamp(-OUTPUT_LIMIT, OUTPUT_LIMIT)
        } else {
            self.diagnostics.nonfinite = self.diagnostics.nonfinite.wrapping_add(1);
            0.0
        }
    }
}

/// Example expressions shown in the editor.
pub const EXAMPLES: [(&str, &str); 5] = [
    ("Soft clip", "tanh(x * ctrl1 * 4)"),
    ("Fold", "abs(x * (1 + ctrl1 * 6)) - 1"),
    ("Ring", "x * sin(phase * 6.2832 * (1 + ctrl1 * 7))"),
    ("Gate", "select(abs(x) > ctrl1, x, 0)"),
    ("Crush", "clamp(x * 8, -1, 1) * 0.5"),
];

#[cfg(test)]
mod tests {
    use super::*;

    fn run(src: &str, x: f32, ctrl1: f32) -> f32 {
        let p = compile(src).unwrap();
        let mut e = Evaluator::new();
        let mut vars = [0.0f32; VARIABLES.len()];
        vars[0] = x;
        vars[5] = ctrl1;
        vars[11] = 48_000.0;
        e.eval(&p, &vars)
    }

    #[test]
    fn arithmetic_precedence_and_functions_match_rust() {
        assert!((run("1 + 2 * 3", 0.0, 0.0) - 7.0).abs() < 1e-6);
        assert!((run("(1 + 2) * 2", 0.0, 0.0) - 6.0).abs() < 1e-6);
        assert_eq!(run("(1 + 2) * 3", 0.0, 0.0), OUTPUT_LIMIT, "outputs are clamped");
        assert!((run("-x * 2 + 1", 0.5, 0.0) - 0.0).abs() < 1e-6);
        assert!((run("tanh(x * ctrl1 * 4)", 0.25, 1.0) - (1.0f32).tanh()).abs() < 1e-3);
        assert!((run("min(x, 0.2) + max(x, 0.7)", 0.5, 0.0) - 0.9).abs() < 1e-6);
        assert!((run("clamp(x * 8, -1, 1)", 0.5, 0.0) - 1.0).abs() < 1e-6);
        assert!((run("select(x > 0.3, 1, -1)", 0.5, 0.0) - 1.0).abs() < 1e-6);
        assert!((run("select(x > 0.3, 1, -1)", 0.1, 0.0) + 1.0).abs() < 1e-6);
        assert!((run("exp(log(x))", 2.5, 0.0) - 2.5).abs() < 1e-5);
        assert!((run("sin(0) + cos(0)", 0.0, 0.0) - 1.0).abs() < 1e-6);
        assert!((run("2e-1 * 10", 0.0, 0.0) - 2.0).abs() < 1e-6);
        assert!((run("x == 0.5", 0.5, 0.0) - 1.0).abs() < 1e-6);
        assert!((run("x != 0.5", 0.5, 0.0)).abs() < 1e-6);
    }

    #[test]
    fn invalid_domains_are_finite_and_counted() {
        let p = compile("x / ctrl1 + log(ctrl1) + exp(x * 1000)").unwrap();
        let mut e = Evaluator::new();
        let mut vars = [0.0f32; VARIABLES.len()];
        vars[0] = 1.0;
        let v = e.eval(&p, &vars);
        assert!(v.is_finite() && v.abs() <= OUTPUT_LIMIT);
        assert_eq!(e.diagnostics.division_by_zero, 1);
        assert_eq!(e.diagnostics.domain_errors, 1);
    }

    #[test]
    fn errors_are_reported_and_limits_enforced() {
        assert!(matches!(compile("foo(x)"), Err(ExprError::Unknown(_))));
        assert!(matches!(compile("y + 1"), Err(ExprError::Unknown(_))));
        assert!(matches!(compile("min(x)"), Err(ExprError::Arity(_, 2))));
        assert!(matches!(compile("1 +"), Err(ExprError::UnexpectedEnd)));
        assert!(matches!(compile("(1 + 2"), Err(ExprError::UnexpectedEnd)));
        assert!(matches!(compile("1 2"), Err(ExprError::UnexpectedToken(..))));
        assert!(matches!(compile("x $ 2"), Err(ExprError::UnexpectedChar('$', 2))));
        assert!(matches!(compile(""), Err(ExprError::UnexpectedEnd)));
        let long = "x".to_owned() + &" + x".repeat(200);
        assert!(matches!(compile(&long), Err(ExprError::TooManyInstructions)));
        let deep = "(".repeat(70) + "x" + &")".repeat(70);
        assert!(compile(&deep).is_ok(), "parentheses do not consume stack");
        let nested = "min(1, ".repeat(70) + "x" + &")".repeat(70);
        assert!(matches!(compile(&nested), Err(ExprError::StackTooDeep)));
        let shallow = "min(".repeat(70) + "x" + &", 1)".repeat(70);
        assert_eq!(compile(&shallow).unwrap().max_depth, 2);
        assert!(matches!(compile(&"x+".repeat(3000)), Err(ExprError::TooLong(_))));
        for (_, src) in EXAMPLES {
            compile(src).unwrap();
        }
    }

    #[test]
    fn program_serializes_with_version() {
        let p = compile("tanh(x)").unwrap();
        assert_eq!(p.version, BYTECODE_VERSION);
        let json = serde_json::to_string(&p).unwrap();
        let back: Program = serde_json::from_str(&json).unwrap();
        assert_eq!(back, p);
        assert_eq!(p.max_depth, 1);
    }
}

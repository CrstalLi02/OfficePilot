//! Excel 公式：分词、相对引用平移，以及常用函数的求值。
//!
//! 写入公式时引擎会算出结果作为缓存值写进单元格，这样不重新计算的程序（预览、
//! 其它读取库）也能看到数字；Excel / WPS 打开时仍会按公式重新计算。
//! 遇到不支持的函数或名称时返回 None，只写公式不写缓存值。

use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Num(f64),
    Text(String),
    Bool(bool),
    Err(String),
    Empty,
}

impl Value {
    fn num(&self) -> Result<f64, Value> {
        match self {
            Value::Num(n) => Ok(*n),
            Value::Bool(b) => Ok(if *b { 1.0 } else { 0.0 }),
            Value::Empty => Ok(0.0),
            Value::Text(t) => t.trim().parse::<f64>().map_err(|_| Value::Err("#VALUE!".into())),
            Value::Err(e) => Err(Value::Err(e.clone())),
        }
    }

    fn text(&self) -> String {
        match self {
            Value::Num(n) => fmt_num(*n),
            Value::Text(t) => t.clone(),
            Value::Bool(b) => if *b { "TRUE" } else { "FALSE" }.into(),
            Value::Err(e) => e.clone(),
            Value::Empty => String::new(),
        }
    }

    fn truthy(&self) -> Result<bool, Value> {
        match self {
            Value::Bool(b) => Ok(*b),
            Value::Text(t) if t.eq_ignore_ascii_case("true") => Ok(true),
            Value::Text(t) if t.eq_ignore_ascii_case("false") => Ok(false),
            other => other.num().map(|n| n != 0.0),
        }
    }
}

pub fn fmt_num(n: f64) -> String {
    if n.fract() == 0.0 && n.abs() < 1e15 {
        format!("{}", n as i64)
    } else {
        let s = format!("{}", (n * 1e12).round() / 1e12);
        if s.len() > 20 { format!("{n}") } else { s }
    }
}

/// 单元格取值：sheet 为空串表示公式所在的工作表
pub trait Lookup {
    fn cell(&self, sheet: &str, row: u32, col: u32) -> Value;
    /// 工作表的最大行、列（整列引用 A:A 用）
    fn extent(&self, sheet: &str) -> (u32, u32);
    fn has_sheet(&self, sheet: &str) -> bool;
}

// ───────────────────────── 引用 ─────────────────────────

pub fn col_to_index(letters: &str) -> Option<u32> {
    if letters.is_empty() || letters.len() > 3 {
        return None;
    }
    let mut n = 0u32;
    for c in letters.chars() {
        if !c.is_ascii_alphabetic() {
            return None;
        }
        n = n * 26 + (c.to_ascii_uppercase() as u32 - 'A' as u32 + 1);
    }
    (n <= 16384).then_some(n)
}

pub fn index_to_col(mut n: u32) -> String {
    let mut s = Vec::new();
    while n > 0 {
        let r = (n - 1) % 26;
        s.push((b'A' + r as u8) as char);
        n = (n - 1) / 26;
    }
    s.iter().rev().collect()
}

/// "B12" / "$B$12" → (12, 2)
pub fn parse_cell(text: &str) -> Option<(u32, u32)> {
    let t = text.replace('$', "");
    let split = t.find(|c: char| c.is_ascii_digit())?;
    let col = col_to_index(&t[..split])?;
    let row: u32 = t[split..].parse().ok()?;
    (row >= 1 && row <= 1_048_576).then_some((row, col))
}

pub fn cell_name(row: u32, col: u32) -> String {
    format!("{}{row}", index_to_col(col))
}

/// "A1:C5" → ((1,1),(5,3))，单个单元格也可以
pub fn parse_range(text: &str) -> Option<((u32, u32), (u32, u32))> {
    let (a, b) = match text.split_once(':') {
        Some((a, b)) => (a, b),
        None => (text, text),
    };
    let (r1, c1) = parse_cell(a.trim())?;
    let (r2, c2) = parse_cell(b.trim())?;
    Some(((r1.min(r2), c1.min(c2)), (r1.max(r2), c1.max(c2))))
}

/// 拆出可选的工作表前缀："'My Sheet'!A1:B2" → (Some("My Sheet"), "A1:B2")
pub fn split_sheet(text: &str) -> (Option<String>, String) {
    match text.rfind('!') {
        Some(pos) => {
            let sheet = text[..pos].trim();
            let sheet = sheet.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')).map(|s| s.replace("''", "'")).unwrap_or_else(|| sheet.to_string());
            (Some(sheet), text[pos + 1..].trim().to_string())
        }
        None => (None, text.trim().to_string()),
    }
}

pub fn quote_sheet(name: &str) -> String {
    if name.chars().all(|c| c.is_alphanumeric() || c == '_') && !name.chars().next().map(|c| c.is_ascii_digit()).unwrap_or(true) && parse_cell(name).is_none() {
        name.to_string()
    } else {
        format!("'{}'", name.replace('\'', "''"))
    }
}

// ───────────────────────── 分词 ─────────────────────────

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Num(f64),
    Str(String),
    /// 引用或名称：可选表名 + 主体（A1、A1:B2、A:A、TRUE、名称）
    Ref(Option<String>, String),
    Func(String),
    Op(String),
    LParen,
    RParen,
    Comma,
    Err(String),
}

/// 分词，同时返回每个记号在原文里的范围
fn tokenize(src: &str) -> Option<Vec<(Tok, usize, usize)>> {
    let chars: Vec<(usize, char)> = src.char_indices().collect();
    let at = |i: usize| chars.get(i).map(|&(_, c)| c);
    let pos = |i: usize| chars.get(i).map(|&(p, _)| p).unwrap_or(src.len());
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i].1;
        let start = i;
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        if c.is_ascii_digit() || (c == '.' && at(i + 1).map(|d| d.is_ascii_digit()).unwrap_or(false)) {
            // 数字，或 1:3 这样的整行引用
            let mut j = i;
            while at(j).map(|d| d.is_ascii_digit() || d == '.').unwrap_or(false) {
                j += 1;
            }
            if matches!(at(j), Some('e') | Some('E')) && at(j + 1).map(|d| d.is_ascii_digit() || d == '+' || d == '-').unwrap_or(false) {
                j += 2;
                while at(j).map(|d| d.is_ascii_digit()).unwrap_or(false) {
                    j += 1;
                }
            }
            let text = &src[pos(i)..pos(j)];
            out.push((Tok::Num(text.parse().ok()?), pos(start), pos(j)));
            i = j;
            continue;
        }
        if c == '"' {
            let mut s = String::new();
            let mut j = i + 1;
            loop {
                match at(j) {
                    Some('"') if at(j + 1) == Some('"') => {
                        s.push('"');
                        j += 2;
                    }
                    Some('"') => {
                        j += 1;
                        break;
                    }
                    Some(ch) => {
                        s.push(ch);
                        j += 1;
                    }
                    None => return None,
                }
            }
            out.push((Tok::Str(s), pos(start), pos(j)));
            i = j;
            continue;
        }
        if c == '#' {
            let mut j = i + 1;
            while at(j).map(|d| d.is_ascii_alphanumeric() || matches!(d, '/' | '!' | '?')).unwrap_or(false) {
                j += 1;
            }
            out.push((Tok::Err(src[pos(i)..pos(j)].to_string()), pos(start), pos(j)));
            i = j;
            continue;
        }
        let ident_char = |d: char| d.is_alphanumeric() || matches!(d, '_' | '.' | '$');
        if c == '\'' || ident_char(c) {
            // 表名（可加引号）或标识符
            let mut sheet = None;
            let mut j = i;
            if c == '\'' {
                let mut s = String::new();
                j += 1;
                loop {
                    match at(j) {
                        Some('\'') if at(j + 1) == Some('\'') => {
                            s.push('\'');
                            j += 2;
                        }
                        Some('\'') => {
                            j += 1;
                            break;
                        }
                        Some(ch) => {
                            s.push(ch);
                            j += 1;
                        }
                        None => return None,
                    }
                }
                if at(j) != Some('!') {
                    return None;
                }
                sheet = Some(s);
                j += 1;
            } else {
                let mut k = j;
                while at(k).map(ident_char).unwrap_or(false) {
                    k += 1;
                }
                if at(k) == Some('!') {
                    sheet = Some(src[pos(j)..pos(k)].to_string());
                    j = k + 1;
                }
            }
            let body_start = j;
            while at(j).map(|d| ident_char(d) || d == ':').unwrap_or(false) {
                // 冒号只在引用里出现（A1:B2）
                if at(j) == Some(':') && !at(j + 1).map(|d| d.is_ascii_alphanumeric() || d == '$').unwrap_or(false) {
                    break;
                }
                j += 1;
            }
            let body = src[pos(body_start)..pos(j)].to_string();
            if body.is_empty() {
                return None;
            }
            if sheet.is_none() && at(j) == Some('(') {
                out.push((Tok::Func(body.to_uppercase()), pos(start), pos(j)));
            } else {
                out.push((Tok::Ref(sheet, body), pos(start), pos(j)));
            }
            i = j;
            continue;
        }
        let two: String = chars[i..chars.len().min(i + 2)].iter().map(|&(_, c)| c).collect();
        if two == "<=" || two == ">=" || two == "<>" {
            out.push((Tok::Op(two), pos(i), pos(i + 2)));
            i += 2;
            continue;
        }
        let tok = match c {
            '(' => Tok::LParen,
            ')' => Tok::RParen,
            ',' | ';' => Tok::Comma,
            '+' | '-' | '*' | '/' | '^' | '&' | '=' | '<' | '>' | '%' => Tok::Op(c.to_string()),
            _ => return None,
        };
        out.push((tok, pos(i), pos(i + 1)));
        i += 1;
    }
    Some(out)
}

/// 平移公式里的相对引用（不带 $ 的行列），用于共享公式展开、复制公式
pub fn shift(formula: &str, drow: i64, dcol: i64) -> Option<String> {
    let toks = tokenize(formula)?;
    let mut out = String::new();
    let mut last = 0;
    for (tok, s, e) in toks {
        if let Tok::Ref(_, body) = &tok {
            let prefix_len = formula[s..e].len() - body.len();
            let shifted: Option<Vec<String>> = body.split(':').map(|part| shift_part(part, drow, dcol)).collect();
            if let Some(parts) = shifted {
                out.push_str(&formula[last..s + prefix_len]);
                out.push_str(&parts.join(":"));
                last = e;
            }
        }
    }
    out.push_str(&formula[last..]);
    Some(out)
}

fn shift_part(part: &str, drow: i64, dcol: i64) -> Option<String> {
    let bytes: Vec<char> = part.chars().collect();
    let mut i = 0;
    let col_abs = bytes.first() == Some(&'$');
    if col_abs {
        i += 1;
    }
    let col_start = i;
    while i < bytes.len() && bytes[i].is_ascii_alphabetic() {
        i += 1;
    }
    let col: String = bytes[col_start..i].iter().collect();
    let row_abs = bytes.get(i) == Some(&'$');
    if row_abs {
        i += 1;
    }
    let row: String = bytes[i..].iter().collect();
    if col.is_empty() && row.is_empty() {
        return None;
    }
    if !row.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let mut out = String::new();
    if !col.is_empty() {
        let idx = col_to_index(&col)? as i64;
        let idx = if col_abs { idx } else { idx + dcol };
        if idx < 1 {
            return None;
        }
        if col_abs {
            out.push('$');
        }
        out.push_str(&index_to_col(idx as u32));
    }
    if !row.is_empty() {
        let r: i64 = row.parse().ok()?;
        let r = if row_abs { r } else { r + drow };
        if r < 1 {
            return None;
        }
        if row_abs {
            out.push('$');
        }
        out.push_str(&r.to_string());
    }
    Some(out)
}

// ───────────────────────── 语法树 ─────────────────────────

#[derive(Debug, Clone)]
enum Expr {
    Lit(Value),
    Cell(Option<String>, u32, u32),
    /// 表名、起止行列；行号 0 表示整列
    Range(Option<String>, u32, u32, u32, u32),
    Unary(String, Box<Expr>),
    Binary(String, Box<Expr>, Box<Expr>),
    Percent(Box<Expr>),
    Call(String, Vec<Expr>),
}

struct Parser {
    toks: Vec<Tok>,
    i: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.i)
    }

    fn next(&mut self) -> Option<Tok> {
        let t = self.toks.get(self.i).cloned();
        self.i += 1;
        t
    }

    fn binary(&mut self, level: usize) -> Option<Expr> {
        const LEVELS: &[&[&str]] = &[&["=", "<>", "<", ">", "<=", ">="], &["&"], &["+", "-"], &["*", "/"], &["^"]];
        if level == LEVELS.len() {
            return self.unary();
        }
        let mut left = self.binary(level + 1)?;
        while let Some(Tok::Op(op)) = self.peek() {
            if !LEVELS[level].contains(&op.as_str()) {
                break;
            }
            let op = op.clone();
            self.i += 1;
            let right = self.binary(level + 1)?;
            left = Expr::Binary(op, Box::new(left), Box::new(right));
        }
        Some(left)
    }

    fn unary(&mut self) -> Option<Expr> {
        if let Some(Tok::Op(op)) = self.peek() {
            if op == "-" || op == "+" {
                let op = op.clone();
                self.i += 1;
                let inner = self.unary()?;
                return Some(Expr::Unary(op, Box::new(inner)));
            }
        }
        let mut e = self.primary()?;
        while let Some(Tok::Op(op)) = self.peek() {
            if op != "%" {
                break;
            }
            self.i += 1;
            e = Expr::Percent(Box::new(e));
        }
        Some(e)
    }

    fn primary(&mut self) -> Option<Expr> {
        match self.next()? {
            Tok::Num(n) => Some(Expr::Lit(Value::Num(n))),
            Tok::Str(s) => Some(Expr::Lit(Value::Text(s))),
            Tok::Err(e) => Some(Expr::Lit(Value::Err(e))),
            Tok::LParen => {
                let e = self.binary(0)?;
                matches!(self.next()?, Tok::RParen).then_some(e)
            }
            Tok::Func(name) => {
                if !matches!(self.next()?, Tok::LParen) {
                    return None;
                }
                let mut args = Vec::new();
                if matches!(self.peek(), Some(Tok::RParen)) {
                    self.i += 1;
                    return Some(Expr::Call(name, args));
                }
                loop {
                    if matches!(self.peek(), Some(Tok::Comma)) {
                        args.push(Expr::Lit(Value::Empty));
                    } else {
                        args.push(self.binary(0)?);
                    }
                    match self.next()? {
                        Tok::Comma => continue,
                        Tok::RParen => break,
                        _ => return None,
                    }
                }
                Some(Expr::Call(name, args))
            }
            Tok::Ref(sheet, body) => {
                let upper = body.to_uppercase();
                if sheet.is_none() && (upper == "TRUE" || upper == "FALSE") {
                    return Some(Expr::Lit(Value::Bool(upper == "TRUE")));
                }
                match body.split_once(':') {
                    None => {
                        let (r, c) = parse_cell(&body)?;
                        Some(Expr::Cell(sheet, r, c))
                    }
                    Some((a, b)) => {
                        if let (Some((r1, c1)), Some((r2, c2))) = (parse_cell(a), parse_cell(b)) {
                            return Some(Expr::Range(sheet, r1.min(r2), c1.min(c2), r2.max(r1), c2.max(c1)));
                        }
                        // 整列 A:C
                        let c1 = col_to_index(&a.replace('$', ""))?;
                        let c2 = col_to_index(&b.replace('$', ""))?;
                        Some(Expr::Range(sheet, 0, c1.min(c2), 0, c1.max(c2)))
                    }
                }
            }
            _ => None,
        }
    }
}

fn parse(formula: &str) -> Option<Expr> {
    let toks: Vec<Tok> = tokenize(formula.trim_start_matches('='))?.into_iter().map(|(t, _, _)| t).collect();
    let mut p = Parser { toks, i: 0 };
    let e = p.binary(0)?;
    (p.i == p.toks.len()).then_some(e)
}

// ───────────────────────── 求值 ─────────────────────────

enum Arg {
    One(Value),
    Many(Vec<Value>, u32),
}

impl Arg {
    fn values(&self) -> Vec<Value> {
        match self {
            Arg::One(v) => vec![v.clone()],
            Arg::Many(v, _) => v.clone(),
        }
    }
}

struct Eval<'a> {
    sheet: &'a str,
    lookup: &'a dyn Lookup,
    unsupported: bool,
}

const MAX_RANGE: u64 = 2_000_000;

impl<'a> Eval<'a> {
    fn sheet_name<'b>(&'b self, s: &'b Option<String>) -> &'b str {
        s.as_deref().unwrap_or(self.sheet)
    }

    fn range(&mut self, sheet: &Option<String>, r1: u32, c1: u32, r2: u32, c2: u32) -> Arg {
        let name = self.sheet_name(sheet).to_string();
        if !self.lookup.has_sheet(&name) {
            return Arg::One(Value::Err("#REF!".into()));
        }
        let (r1, r2) = if r1 == 0 { (1, self.lookup.extent(&name).0.max(1)) } else { (r1, r2) };
        if (r2 - r1 + 1) as u64 * (c2 - c1 + 1) as u64 > MAX_RANGE {
            self.unsupported = true;
            return Arg::One(Value::Err("#REF!".into()));
        }
        let mut out = Vec::new();
        for r in r1..=r2 {
            for c in c1..=c2 {
                out.push(self.lookup.cell(&name, r, c));
            }
        }
        Arg::Many(out, c2 - c1 + 1)
    }

    fn arg(&mut self, e: &Expr) -> Arg {
        match e {
            Expr::Range(s, r1, c1, r2, c2) => self.range(s, *r1, *c1, *r2, *c2),
            other => Arg::One(self.eval(other)),
        }
    }

    fn eval(&mut self, e: &Expr) -> Value {
        match e {
            Expr::Lit(v) => v.clone(),
            Expr::Cell(s, r, c) => {
                let name = self.sheet_name(s).to_string();
                if !self.lookup.has_sheet(&name) {
                    return Value::Err("#REF!".into());
                }
                self.lookup.cell(&name, *r, *c)
            }
            Expr::Range(s, r1, c1, r2, c2) => match self.range(s, *r1, *c1, *r2, *c2) {
                // 单元格区域当单值用时取第一个（隐式交集的简化）
                Arg::Many(v, _) if v.len() == 1 => v[0].clone(),
                Arg::Many(..) => Value::Err("#VALUE!".into()),
                Arg::One(v) => v,
            },
            Expr::Percent(inner) => match self.eval(inner).num() {
                Ok(n) => Value::Num(n / 100.0),
                Err(e) => e,
            },
            Expr::Unary(op, inner) => match self.eval(inner).num() {
                Ok(n) => Value::Num(if op == "-" { -n } else { n }),
                Err(e) => e,
            },
            Expr::Binary(op, a, b) => {
                let (a, b) = (self.eval(a), self.eval(b));
                if let Value::Err(_) = a {
                    return a;
                }
                if let Value::Err(_) = b {
                    return b;
                }
                binary(op, &a, &b)
            }
            Expr::Call(name, args) => self.call(name, args),
        }
    }

    fn nums(&mut self, args: &[Expr], strict_single: bool) -> Result<Vec<f64>, Value> {
        let mut out = Vec::new();
        for a in args {
            match self.arg(a) {
                Arg::One(v) => match v {
                    Value::Err(_) => return Err(v),
                    Value::Text(_) if strict_single => out.push(v.num()?),
                    Value::Text(_) | Value::Empty => {}
                    v => out.push(v.num()?),
                },
                Arg::Many(vs, _) => {
                    for v in vs {
                        match v {
                            Value::Num(n) => out.push(n),
                            Value::Err(_) => return Err(v),
                            _ => {}
                        }
                    }
                }
            }
        }
        Ok(out)
    }

    fn call(&mut self, name: &str, args: &[Expr]) -> Value {
        let name = name.trim_start_matches("_XLFN.").to_string();
        macro_rules! try_v {
            ($e:expr) => {
                match $e {
                    Ok(v) => v,
                    Err(e) => return e,
                }
            };
        }
        let n_args = args.len();
        let num_arg = |ev: &mut Self, i: usize| -> Result<f64, Value> { args.get(i).map(|a| ev.eval(a)).unwrap_or(Value::Empty).num() };
        match name.as_str() {
            "SUM" => Value::Num(try_v!(self.nums(args, true)).iter().sum()),
            "AVERAGE" => {
                let v = try_v!(self.nums(args, true));
                if v.is_empty() { Value::Err("#DIV/0!".into()) } else { Value::Num(v.iter().sum::<f64>() / v.len() as f64) }
            }
            "MIN" => Value::Num(try_v!(self.nums(args, true)).into_iter().fold(None, |m: Option<f64>, x| Some(m.map_or(x, |m| m.min(x)))).unwrap_or(0.0)),
            "MAX" => Value::Num(try_v!(self.nums(args, true)).into_iter().fold(None, |m: Option<f64>, x| Some(m.map_or(x, |m| m.max(x)))).unwrap_or(0.0)),
            "MEDIAN" => {
                let mut v = try_v!(self.nums(args, true));
                if v.is_empty() {
                    return Value::Err("#NUM!".into());
                }
                v.sort_by(|a, b| a.partial_cmp(b).unwrap());
                let m = v.len() / 2;
                Value::Num(if v.len() % 2 == 1 { v[m] } else { (v[m - 1] + v[m]) / 2.0 })
            }
            "PRODUCT" => Value::Num(try_v!(self.nums(args, true)).iter().product()),
            "COUNT" => {
                let mut c = 0;
                for a in args {
                    for v in self.arg(a).values() {
                        if matches!(v, Value::Num(_)) {
                            c += 1;
                        }
                    }
                }
                Value::Num(c as f64)
            }
            "COUNTA" => {
                let mut c = 0;
                for a in args {
                    for v in self.arg(a).values() {
                        if !matches!(v, Value::Empty) {
                            c += 1;
                        }
                    }
                }
                Value::Num(c as f64)
            }
            "COUNTBLANK" => {
                let mut c = 0;
                for a in args {
                    for v in self.arg(a).values() {
                        if matches!(v, Value::Empty) || v == Value::Text(String::new()) {
                            c += 1;
                        }
                    }
                }
                Value::Num(c as f64)
            }
            "ABS" => Value::Num(try_v!(num_arg(self, 0)).abs()),
            "INT" => Value::Num(try_v!(num_arg(self, 0)).floor()),
            "SQRT" => {
                let n = try_v!(num_arg(self, 0));
                if n < 0.0 { Value::Err("#NUM!".into()) } else { Value::Num(n.sqrt()) }
            }
            "POWER" => Value::Num(try_v!(num_arg(self, 0)).powf(try_v!(num_arg(self, 1)))),
            "MOD" => {
                let (a, b) = (try_v!(num_arg(self, 0)), try_v!(num_arg(self, 1)));
                if b == 0.0 { Value::Err("#DIV/0!".into()) } else { Value::Num(a - b * (a / b).floor()) }
            }
            "ROUND" | "ROUNDUP" | "ROUNDDOWN" => {
                let x = try_v!(num_arg(self, 0));
                let d = if n_args > 1 { try_v!(num_arg(self, 1)) } else { 0.0 } as i32;
                let f = 10f64.powi(d);
                let scaled = x * f;
                let r = match name.as_str() {
                    "ROUND" => (scaled.abs() + 0.5 - 1e-9).floor() * scaled.signum(),
                    "ROUNDUP" => scaled.abs().ceil() * scaled.signum(),
                    _ => scaled.abs().floor() * scaled.signum(),
                };
                Value::Num(r / f)
            }
            "IF" => {
                let cond = try_v!(args.first().map(|a| self.eval(a)).unwrap_or(Value::Empty).truthy());
                if cond {
                    args.get(1).map(|a| self.eval(a)).unwrap_or(Value::Bool(true))
                } else {
                    args.get(2).map(|a| self.eval(a)).unwrap_or(Value::Bool(false))
                }
            }
            "IFERROR" => {
                let v = args.first().map(|a| self.eval(a)).unwrap_or(Value::Empty);
                if matches!(v, Value::Err(_)) { args.get(1).map(|a| self.eval(a)).unwrap_or(Value::Empty) } else { v }
            }
            "AND" | "OR" => {
                let mut vals = Vec::new();
                for a in args {
                    for v in self.arg(a).values() {
                        if matches!(v, Value::Empty) {
                            continue;
                        }
                        vals.push(try_v!(v.truthy()));
                    }
                }
                Value::Bool(if name == "AND" { vals.iter().all(|&b| b) } else { vals.iter().any(|&b| b) })
            }
            "NOT" => Value::Bool(!try_v!(args.first().map(|a| self.eval(a)).unwrap_or(Value::Empty).truthy())),
            "CONCATENATE" | "CONCAT" => {
                let mut s = String::new();
                for a in args {
                    for v in self.arg(a).values() {
                        if let Value::Err(_) = v {
                            return v;
                        }
                        s.push_str(&v.text());
                    }
                }
                Value::Text(s)
            }
            "LEN" => Value::Num(args.first().map(|a| self.eval(a)).unwrap_or(Value::Empty).text().chars().count() as f64),
            "UPPER" => Value::Text(args.first().map(|a| self.eval(a)).unwrap_or(Value::Empty).text().to_uppercase()),
            "LOWER" => Value::Text(args.first().map(|a| self.eval(a)).unwrap_or(Value::Empty).text().to_lowercase()),
            "TRIM" => Value::Text(args.first().map(|a| self.eval(a)).unwrap_or(Value::Empty).text().split_whitespace().collect::<Vec<_>>().join(" ")),
            "LEFT" | "RIGHT" => {
                let s: Vec<char> = args.first().map(|a| self.eval(a)).unwrap_or(Value::Empty).text().chars().collect();
                let n = if n_args > 1 { try_v!(num_arg(self, 1)) as usize } else { 1 }.min(s.len());
                Value::Text(if name == "LEFT" { s[..n].iter().collect() } else { s[s.len() - n..].iter().collect() })
            }
            "MID" => {
                let s: Vec<char> = args.first().map(|a| self.eval(a)).unwrap_or(Value::Empty).text().chars().collect();
                let start = (try_v!(num_arg(self, 1)) as usize).max(1) - 1;
                let len = try_v!(num_arg(self, 2)).max(0.0) as usize;
                Value::Text(s.iter().skip(start).take(len).collect())
            }
            "SUMIF" | "COUNTIF" | "AVERAGEIF" => {
                let Some(range_e) = args.first() else { return Value::Err("#VALUE!".into()) };
                let range = self.arg(range_e).values();
                let crit = args.get(1).map(|a| self.eval(a)).unwrap_or(Value::Empty);
                let sum_range = if name != "COUNTIF" && n_args > 2 { self.arg(&args[2]).values() } else { range.clone() };
                let matcher = Criteria::new(&crit);
                let mut total = 0.0;
                let mut count = 0;
                for (i, v) in range.iter().enumerate() {
                    if matcher.matches(v) {
                        count += 1;
                        if let Some(Value::Num(n)) = sum_range.get(i) {
                            total += n;
                        }
                    }
                }
                match name.as_str() {
                    "COUNTIF" => Value::Num(count as f64),
                    "SUMIF" => Value::Num(total),
                    _ if count == 0 => Value::Err("#DIV/0!".into()),
                    _ => Value::Num(total / count as f64),
                }
            }
            "SUMIFS" | "COUNTIFS" | "AVERAGEIFS" => {
                let (target, pairs) = if name == "COUNTIFS" { (None, args) } else { (args.first(), args.get(1..).unwrap_or(&[])) };
                if pairs.len() % 2 != 0 {
                    return Value::Err("#VALUE!".into());
                }
                let target_vals = target.map(|t| self.arg(t).values());
                let mut conds = Vec::new();
                for pair in pairs.chunks(2) {
                    let range = self.arg(&pair[0]).values();
                    let crit = self.eval(&pair[1]);
                    conds.push((range, Criteria::new(&crit)));
                }
                let len = conds.first().map(|c| c.0.len()).unwrap_or(0);
                let mut total = 0.0;
                let mut count = 0;
                for i in 0..len {
                    if conds.iter().all(|(r, m)| r.get(i).map(|v| m.matches(v)).unwrap_or(false)) {
                        count += 1;
                        if let Some(Value::Num(n)) = target_vals.as_ref().and_then(|t| t.get(i)) {
                            total += n;
                        }
                    }
                }
                match name.as_str() {
                    "COUNTIFS" => Value::Num(count as f64),
                    "SUMIFS" => Value::Num(total),
                    _ if count == 0 => Value::Err("#DIV/0!".into()),
                    _ => Value::Num(total / count as f64),
                }
            }
            "SUMPRODUCT" => {
                let arrays: Vec<Vec<Value>> = args.iter().map(|a| self.arg(a).values()).collect();
                let len = arrays.first().map(Vec::len).unwrap_or(0);
                if arrays.iter().any(|a| a.len() != len) {
                    return Value::Err("#VALUE!".into());
                }
                let mut total = 0.0;
                for i in 0..len {
                    let mut p = 1.0;
                    for a in &arrays {
                        p *= match &a[i] {
                            Value::Num(n) => *n,
                            _ => 0.0,
                        };
                    }
                    total += p;
                }
                Value::Num(total)
            }
            "VLOOKUP" | "HLOOKUP" => {
                let key = args.first().map(|a| self.eval(a)).unwrap_or(Value::Empty);
                let Some(table) = args.get(1) else { return Value::Err("#VALUE!".into()) };
                let (vals, width) = match self.arg(table) {
                    Arg::Many(v, w) => (v, w as usize),
                    Arg::One(v) => (vec![v], 1),
                };
                let idx = try_v!(num_arg(self, 2)) as usize;
                let exact = n_args > 3 && !try_v!(self.eval(&args[3]).truthy());
                if !exact {
                    // 近似匹配需要排序数据，交给 Excel 计算
                    self.unsupported = true;
                    return Value::Empty;
                }
                let height = vals.len() / width.max(1);
                if name == "VLOOKUP" {
                    if idx < 1 || idx > width {
                        return Value::Err("#REF!".into());
                    }
                    for r in 0..height {
                        if loose_eq(&vals[r * width], &key) {
                            return vals[r * width + idx - 1].clone();
                        }
                    }
                } else {
                    if idx < 1 || idx > height {
                        return Value::Err("#REF!".into());
                    }
                    for c in 0..width {
                        if loose_eq(&vals[c], &key) {
                            return vals[(idx - 1) * width + c].clone();
                        }
                    }
                }
                Value::Err("#N/A".into())
            }
            "INDEX" => {
                let Some(table) = args.first() else { return Value::Err("#VALUE!".into()) };
                let (vals, width) = match self.arg(table) {
                    Arg::Many(v, w) => (v, w as usize),
                    Arg::One(v) => (vec![v], 1),
                };
                let height = vals.len() / width.max(1);
                let r = try_v!(num_arg(self, 1)) as usize;
                let c = if n_args > 2 { try_v!(num_arg(self, 2)) as usize } else { 1 };
                let (r, c) = if height == 1 && n_args == 2 { (1, r) } else { (r, c) };
                if r < 1 || c < 1 || r > height || c > width {
                    return Value::Err("#REF!".into());
                }
                vals[(r - 1) * width + c - 1].clone()
            }
            "MATCH" => {
                let key = args.first().map(|a| self.eval(a)).unwrap_or(Value::Empty);
                let vals = args.get(1).map(|a| self.arg(a).values()).unwrap_or_default();
                let mode = if n_args > 2 { try_v!(num_arg(self, 2)) } else { 1.0 };
                if mode != 0.0 {
                    self.unsupported = true;
                    return Value::Empty;
                }
                vals.iter().position(|v| loose_eq(v, &key)).map(|p| Value::Num(p as f64 + 1.0)).unwrap_or(Value::Err("#N/A".into()))
            }
            _ => {
                self.unsupported = true;
                Value::Empty
            }
        }
    }
}

fn loose_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Text(x), Value::Text(y)) => x.eq_ignore_ascii_case(y),
        (Value::Num(x), Value::Num(y)) => (x - y).abs() < 1e-12,
        (Value::Bool(x), Value::Bool(y)) => x == y,
        _ => false,
    }
}

fn compare(a: &Value, b: &Value) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let rank = |v: &Value| match v {
        Value::Num(_) | Value::Empty => 0,
        Value::Text(_) => 1,
        Value::Bool(_) => 2,
        Value::Err(_) => 3,
    };
    match (a, b) {
        (Value::Text(x), Value::Text(y)) => x.to_lowercase().cmp(&y.to_lowercase()),
        (Value::Empty, Value::Text(y)) => String::new().cmp(&y.to_lowercase()),
        (Value::Text(x), Value::Empty) => x.to_lowercase().cmp(&String::new()),
        (Value::Bool(x), Value::Bool(y)) => x.cmp(y),
        _ if rank(a) == 0 && rank(b) == 0 => a.num().unwrap_or(0.0).partial_cmp(&b.num().unwrap_or(0.0)).unwrap_or(Ordering::Equal),
        _ => rank(a).cmp(&rank(b)),
    }
}

fn binary(op: &str, a: &Value, b: &Value) -> Value {
    use std::cmp::Ordering::*;
    match op {
        "&" => Value::Text(format!("{}{}", a.text(), b.text())),
        "=" | "<>" | "<" | ">" | "<=" | ">=" => {
            let ord = compare(a, b);
            Value::Bool(match op {
                "=" => ord == Equal,
                "<>" => ord != Equal,
                "<" => ord == Less,
                ">" => ord == Greater,
                "<=" => ord != Greater,
                _ => ord != Less,
            })
        }
        _ => {
            let (x, y) = match (a.num(), b.num()) {
                (Ok(x), Ok(y)) => (x, y),
                (Err(e), _) | (_, Err(e)) => return e,
            };
            match op {
                "+" => Value::Num(x + y),
                "-" => Value::Num(x - y),
                "*" => Value::Num(x * y),
                "/" if y == 0.0 => Value::Err("#DIV/0!".into()),
                "/" => Value::Num(x / y),
                "^" => Value::Num(x.powf(y)),
                _ => Value::Err("#VALUE!".into()),
            }
        }
    }
}

/// SUMIF / COUNTIF 的条件：">5"、"<>北京"、"=0"、"华东"、"A*"
struct Criteria {
    op: &'static str,
    target: Value,
    pattern: Option<regex_lite::Pattern>,
}

mod regex_lite {
    /// 只支持 * 和 ? 的通配符
    pub struct Pattern(Vec<char>);

    impl Pattern {
        pub fn new(p: &str) -> Self {
            Self(p.to_lowercase().chars().collect())
        }

        pub fn matches(&self, text: &str) -> bool {
            let t: Vec<char> = text.to_lowercase().chars().collect();
            fn go(p: &[char], t: &[char]) -> bool {
                match p.first() {
                    None => t.is_empty(),
                    Some('*') => (0..=t.len()).any(|i| go(&p[1..], &t[i..])),
                    Some('?') => !t.is_empty() && go(&p[1..], &t[1..]),
                    Some(c) => t.first() == Some(c) && go(&p[1..], &t[1..]),
                }
            }
            go(&self.0, &t)
        }
    }
}

impl Criteria {
    fn new(crit: &Value) -> Self {
        let text = match crit {
            Value::Text(t) => t.clone(),
            other => return Self { op: "=", target: other.clone(), pattern: None },
        };
        let (op, rest) = ["<=", ">=", "<>", "<", ">", "="].iter().find_map(|op| text.strip_prefix(op).map(|r| (*op, r))).unwrap_or(("=", text.as_str()));
        let target = match rest.trim().parse::<f64>() {
            Ok(n) => Value::Num(n),
            Err(_) => Value::Text(rest.to_string()),
        };
        let pattern = match &target {
            Value::Text(t) if (op == "=" || op == "<>") && (t.contains('*') || t.contains('?')) => Some(regex_lite::Pattern::new(t)),
            _ => None,
        };
        Self { op, target, pattern }
    }

    fn matches(&self, v: &Value) -> bool {
        if let Some(p) = &self.pattern {
            let hit = matches!(v, Value::Text(_)) && p.matches(&v.text());
            return if self.op == "<>" { !hit } else { hit };
        }
        // 数字条件只匹配数字，文字条件只匹配文字（与 Excel 一致）
        let comparable = matches!((&self.target, v), (Value::Num(_), Value::Num(_)) | (Value::Text(_), Value::Text(_)) | (Value::Bool(_), Value::Bool(_)))
            || (matches!(self.target, Value::Text(ref t) if t.is_empty()) && matches!(v, Value::Empty));
        let ord = compare(v, &self.target);
        use std::cmp::Ordering::*;
        match self.op {
            "=" => comparable && ord == Equal,
            "<>" => !(comparable && ord == Equal),
            "<" => comparable && ord == Less,
            ">" => comparable && ord == Greater,
            "<=" => comparable && ord != Greater,
            _ => comparable && ord != Less,
        }
    }
}

/// 与 Excel 一样保留 15 位有效数字，避免 0.7000000000000001 这样的浮点尾巴
fn round15(n: f64) -> f64 {
    if n == 0.0 || !n.is_finite() {
        return n;
    }
    let digits = 15 - n.abs().log10().ceil() as i32;
    if !(-300..=300).contains(&digits) {
        return n;
    }
    let f = 10f64.powi(digits);
    let r = (n * f).round() / f;
    if r.is_finite() { r } else { n }
}

/// 计算公式。返回 None 表示含不支持的函数或语法，调用方只写公式、不写缓存值。
pub fn evaluate(formula: &str, sheet: &str, lookup: &dyn Lookup) -> Option<Value> {
    let expr = parse(formula)?;
    let mut ev = Eval { sheet, lookup, unsupported: false };
    let v = match ev.arg(&expr) {
        Arg::One(v) => v,
        Arg::Many(v, _) => v.into_iter().next().unwrap_or(Value::Empty),
    };
    (!ev.unsupported).then_some(match v {
        Value::Num(n) if !n.is_finite() => Value::Err("#NUM!".into()),
        Value::Num(n) => Value::Num(round15(n)),
        v => v,
    })
}

/// 公式引用到的单元格（用于依赖顺序）；区域只返回起止范围
pub fn references(formula: &str) -> Vec<(Option<String>, (u32, u32), (u32, u32))> {
    let Some(toks) = tokenize(formula.trim_start_matches('=')) else { return Vec::new() };
    toks.into_iter()
        .filter_map(|(t, _, _)| match t {
            Tok::Ref(sheet, body) => parse_range(&body).map(|(a, b)| (sheet, a, b)),
            _ => None,
        })
        .collect()
}

/// 测试与简单场景用的查表
pub struct MapLookup(pub HashMap<(String, u32, u32), Value>);

impl Lookup for MapLookup {
    fn cell(&self, sheet: &str, row: u32, col: u32) -> Value {
        self.0.get(&(sheet.to_string(), row, col)).cloned().unwrap_or(Value::Empty)
    }
    fn extent(&self, sheet: &str) -> (u32, u32) {
        self.0.keys().filter(|k| k.0 == sheet).fold((0, 0), |(r, c), k| (r.max(k.1), c.max(k.2)))
    }
    fn has_sheet(&self, sheet: &str) -> bool {
        self.0.keys().any(|k| k.0 == sheet)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn data() -> MapLookup {
        let mut m = HashMap::new();
        let rows = [("华东", 100.0, "A"), ("华北", 50.0, "B"), ("华东", 30.0, "A"), ("西南", 20.0, "C")];
        for (i, (region, amount, cat)) in rows.iter().enumerate() {
            let r = i as u32 + 2;
            m.insert(("Data".into(), r, 1), Value::Text(region.to_string()));
            m.insert(("Data".into(), r, 2), Value::Num(*amount));
            m.insert(("Data".into(), r, 3), Value::Text(cat.to_string()));
        }
        m.insert(("Data".into(), 1, 1), Value::Text("地区".into()));
        m.insert(("Other Sheet".into(), 1, 1), Value::Num(7.0));
        MapLookup(m)
    }

    fn ev(f: &str) -> Option<Value> {
        evaluate(f, "Data", &data())
    }

    #[test]
    fn evaluates_common_functions() {
        assert_eq!(ev("=SUM(B2:B5)"), Some(Value::Num(200.0)));
        assert_eq!(ev("=SUMIF(A2:A5,\"华东\",B2:B5)"), Some(Value::Num(130.0)));
        assert_eq!(ev("=COUNTIF(B:B,\">=30\")"), Some(Value::Num(3.0)));
        assert_eq!(ev("=SUMIFS(B2:B5,A2:A5,\"华*\",C2:C5,\"A\")"), Some(Value::Num(130.0)));
        assert_eq!(ev("=AVERAGE(B2:B5)*2+1"), Some(Value::Num(101.0)));
        assert_eq!(ev("=ROUND(B3/3,2)"), Some(Value::Num(16.67)));
        assert_eq!(ev("=IF(B2>B3,\"高\",\"低\")&\"!\""), Some(Value::Text("高!".into())));
        assert_eq!(ev("='Other Sheet'!A1*10%"), Some(Value::Num(0.7)));
        assert_eq!(ev("=VLOOKUP(\"华北\",A2:C5,2,FALSE)"), Some(Value::Num(50.0)));
        assert_eq!(ev("=INDEX(B2:B5,MATCH(\"西南\",A2:A5,0))"), Some(Value::Num(20.0)));
        assert_eq!(ev("=B2/0"), Some(Value::Err("#DIV/0!".into())));
        assert_eq!(ev("=IFERROR(B2/0,0)"), Some(Value::Num(0.0)));
        assert_eq!(ev("=-B2^2"), Some(Value::Num(10000.0)));
        assert_eq!(ev("=XNPV(1,2)"), None);
        assert_eq!(ev("=Missing!A1"), Some(Value::Err("#REF!".into())));
    }

    #[test]
    fn shifts_relative_refs() {
        assert_eq!(shift("SUM(B2:B5)*$C$1+D3", 1, 2).unwrap(), "SUM(D3:D6)*$C$1+F4");
        assert_eq!(shift("'My Sheet'!A1&\"A1\"", 2, 0).unwrap(), "'My Sheet'!A3&\"A1\"");
        assert_eq!(references("=SUM(Data!B2:B5)+C1").len(), 2);
    }

    #[test]
    fn refs_and_quoting() {
        assert_eq!(parse_cell("$AB$12"), Some((12, 28)));
        assert_eq!(cell_name(12, 28), "AB12");
        assert_eq!(quote_sheet("Sheet1"), "Sheet1");
        assert_eq!(quote_sheet("销售 数据"), "'销售 数据'");
        assert_eq!(split_sheet("'a''b'!A1"), (Some("a'b".into()), "A1".into()));
    }
}

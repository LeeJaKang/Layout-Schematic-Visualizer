//! Whitespace tokenizer shared by the LEF and DEF readers: `#` comments, "quoted strings".

pub struct Tokens<'a> {
    pub text: &'a [u8],
    pub pos: usize,
    pub line: usize,
}

#[inline]
fn is_ws(c: u8) -> bool {
    c == b' ' || c == b'\t' || c == b'\n' || c == b'\r'
}

impl<'a> Tokens<'a> {
    pub fn new(text: &'a [u8]) -> Self {
        Tokens { text, pos: 0, line: 1 }
    }
    /// next token, or None at the end
    pub fn next(&mut self) -> Option<&'a [u8]> {
        let t = self.text;
        let n = t.len();
        loop {
            while self.pos < n && is_ws(t[self.pos]) {
                if t[self.pos] == b'\n' {
                    self.line += 1;
                }
                self.pos += 1;
            }
            if self.pos < n && t[self.pos] == b'#' {
                while self.pos < n && t[self.pos] != b'\n' {
                    self.pos += 1;
                }
                continue;
            }
            break;
        }
        if self.pos >= n {
            return None;
        }
        if t[self.pos] == b'"' {
            let st = self.pos + 1;
            let e = memchr::memchr(b'"', &t[st..]).map(|k| st + k).unwrap_or(n);
            self.line += t[st..e].iter().filter(|&&c| c == b'\n').count();
            self.pos = (e + 1).min(n);
            return Some(&t[st..e]);
        }
        let st = self.pos;
        while self.pos < n && !is_ws(t[self.pos]) {
            self.pos += 1;
        }
        Some(&t[st..self.pos])
    }
    pub fn peek(&mut self) -> Option<&'a [u8]> {
        let (p, l) = (self.pos, self.line);
        let r = self.next();
        self.pos = p;
        self.line = l;
        r
    }
    pub fn skip_to(&mut self, tok: &[u8]) {
        while let Some(t) = self.next() {
            if t == tok {
                return;
            }
        }
    }
}

/// number token (integers fast, otherwise f64 parse); None if not a number
#[inline]
pub fn num(t: &[u8]) -> Option<f64> {
    if t.is_empty() {
        return None;
    }
    let (neg, d) = if t[0] == b'-' { (true, &t[1..]) } else if t[0] == b'+' { (false, &t[1..]) } else { (false, t) };
    if !d.is_empty() && d.len() < 16 && d.iter().all(|c| c.is_ascii_digit()) {
        let mut v: i64 = 0;
        for &c in d {
            v = v * 10 + (c - b'0') as i64;
        }
        return Some(if neg { -v as f64 } else { v as f64 });
    }
    std::str::from_utf8(t).ok()?.parse::<f64>().ok()
}

pub fn s(t: &[u8]) -> String {
    String::from_utf8_lossy(t).into_owned()
}

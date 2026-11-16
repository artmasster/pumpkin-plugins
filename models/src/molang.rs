//! The part of Molang that animation keyframes use: numbers, arithmetic, comparisons, the
//! `math.` functions and the time of the animation. Angles are in degrees, as in Blockbench.
//!
//! An expression is read once and then worked out for every tick of the animation while the
//! model loads; nothing here runs while the server ticks.

#[derive(Clone, Debug, PartialEq)]
pub enum Expr {
    Num(f32),
    /// Seconds into the animation (`query.anim_time`, `q.anim_time`, `query.life_time`).
    Time,
    Neg(Box<Expr>),
    Not(Box<Expr>),
    Bin(char, Box<Expr>, Box<Expr>),
    /// A comparison or a logic operator: `<`, `>`, `l` (<=), `g` (>=), `=`, `!`, `&`, `|`.
    Cmp(char, Box<Expr>, Box<Expr>),
    If(Box<Expr>, Box<Expr>, Box<Expr>),
    Call(Func, Vec<Expr>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Func {
    Sin,
    Cos,
    Abs,
    Min,
    Max,
    Pow,
    Sqrt,
    Floor,
    Ceil,
    Round,
    Trunc,
    Clamp,
    Lerp,
    Mod,
    Exp,
    Ln,
}

struct Parser<'a> {
    s: &'a [u8],
    at: usize,
}

/// Read an expression. `None` when it uses something this file does not know.
pub fn parse(text: &str) -> Option<Expr> {
    let lower = text.trim().to_ascii_lowercase();
    // `return x;` and a trailing `;` are ways of writing the same thing
    let body = lower.strip_prefix("return ").unwrap_or(&lower).trim_end_matches(';').trim();
    if body.is_empty() {
        return Some(Expr::Num(0.0));
    }
    let mut p = Parser { s: body.as_bytes(), at: 0 };
    let e = p.ternary()?;
    p.space();
    (p.at == p.s.len()).then_some(e)
}

impl Parser<'_> {
    fn space(&mut self) {
        while self.s.get(self.at).is_some_and(|b| b.is_ascii_whitespace()) {
            self.at += 1;
        }
    }
    fn eat(&mut self, token: &str) -> bool {
        self.space();
        if self.s[self.at..].starts_with(token.as_bytes()) {
            self.at += token.len();
            return true;
        }
        false
    }
    fn ternary(&mut self) -> Option<Expr> {
        let cond = self.or()?;
        if self.eat("?") {
            let yes = self.ternary()?;
            let no = if self.eat(":") { self.ternary()? } else { Expr::Num(0.0) };
            return Some(Expr::If(Box::new(cond), Box::new(yes), Box::new(no)));
        }
        Some(cond)
    }
    fn or(&mut self) -> Option<Expr> {
        let mut left = self.and()?;
        while self.eat("||") {
            left = Expr::Cmp('|', Box::new(left), Box::new(self.and()?));
        }
        Some(left)
    }
    fn and(&mut self) -> Option<Expr> {
        let mut left = self.compare()?;
        while self.eat("&&") {
            left = Expr::Cmp('&', Box::new(left), Box::new(self.compare()?));
        }
        Some(left)
    }
    fn compare(&mut self) -> Option<Expr> {
        let left = self.sum()?;
        for (token, op) in [("<=", 'l'), (">=", 'g'), ("==", '='), ("!=", '!'), ("<", '<'), (">", '>')] {
            if self.eat(token) {
                return Some(Expr::Cmp(op, Box::new(left), Box::new(self.sum()?)));
            }
        }
        Some(left)
    }
    fn sum(&mut self) -> Option<Expr> {
        let mut left = self.product()?;
        loop {
            let op = if self.eat("+") {
                '+'
            } else if self.eat("-") {
                '-'
            } else {
                return Some(left);
            };
            left = Expr::Bin(op, Box::new(left), Box::new(self.product()?));
        }
    }
    fn product(&mut self) -> Option<Expr> {
        let mut left = self.unary()?;
        loop {
            let op = if self.eat("*") {
                '*'
            } else if self.eat("/") {
                '/'
            } else {
                return Some(left);
            };
            left = Expr::Bin(op, Box::new(left), Box::new(self.unary()?));
        }
    }
    fn unary(&mut self) -> Option<Expr> {
        if self.eat("-") {
            return Some(Expr::Neg(Box::new(self.unary()?)));
        }
        if self.eat("+") {
            return self.unary();
        }
        if self.eat("!") {
            return Some(Expr::Not(Box::new(self.unary()?)));
        }
        self.atom()
    }
    fn atom(&mut self) -> Option<Expr> {
        self.space();
        if self.eat("(") {
            let e = self.ternary()?;
            return self.eat(")").then_some(e);
        }
        let start = self.at;
        if self.s.get(self.at).is_some_and(|b| b.is_ascii_digit() || *b == b'.') {
            while self.s.get(self.at).is_some_and(|b| b.is_ascii_digit() || *b == b'.') {
                self.at += 1;
            }
            let text = std::str::from_utf8(&self.s[start..self.at]).ok()?;
            // 1.5f is how some exporters write a number
            self.eat("f");
            return text.parse().ok().map(Expr::Num);
        }
        while self.s.get(self.at).is_some_and(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.')) {
            self.at += 1;
        }
        let name = std::str::from_utf8(&self.s[start..self.at]).ok()?;
        match name {
            "query.anim_time" | "q.anim_time" | "query.life_time" | "q.life_time" | "query.time_stamp" | "q.time_stamp" => return Some(Expr::Time),
            "math.pi" => return Some(Expr::Num(std::f32::consts::PI)),
            "true" => return Some(Expr::Num(1.0)),
            "false" => return Some(Expr::Num(0.0)),
            "query.delta_time" | "q.delta_time" => return Some(Expr::Num(0.05)),
            _ => {}
        }
        let func = match name.strip_prefix("math.")? {
            "sin" => Func::Sin,
            "cos" => Func::Cos,
            "abs" => Func::Abs,
            "min" => Func::Min,
            "max" => Func::Max,
            "pow" => Func::Pow,
            "sqrt" => Func::Sqrt,
            "floor" => Func::Floor,
            "ceil" => Func::Ceil,
            "round" => Func::Round,
            "trunc" => Func::Trunc,
            "clamp" => Func::Clamp,
            "lerp" => Func::Lerp,
            "mod" => Func::Mod,
            "exp" => Func::Exp,
            "ln" => Func::Ln,
            _ => return None,
        };
        let mut args = Vec::new();
        if !self.eat("(") {
            return None;
        }
        if !self.eat(")") {
            loop {
                args.push(self.ternary()?);
                if self.eat(")") {
                    break;
                }
                if !self.eat(",") {
                    return None;
                }
            }
        }
        let wanted = match func {
            Func::Clamp | Func::Lerp => 3,
            Func::Min | Func::Max | Func::Pow | Func::Mod => 2,
            _ => 1,
        };
        (args.len() == wanted).then_some(Expr::Call(func, args))
    }
}

impl Expr {
    /// The value at `time` seconds into the animation. Never NaN or infinite.
    pub fn eval(&self, time: f32) -> f32 {
        let v = match self {
            Expr::Num(n) => *n,
            Expr::Time => time,
            Expr::Neg(e) => -e.eval(time),
            Expr::Not(e) => f32::from(e.eval(time) == 0.0),
            Expr::Bin(op, a, b) => {
                let (a, b) = (a.eval(time), b.eval(time));
                match op {
                    '+' => a + b,
                    '-' => a - b,
                    '*' => a * b,
                    _ => {
                        if b == 0.0 {
                            0.0
                        } else {
                            a / b
                        }
                    }
                }
            }
            Expr::Cmp(op, a, b) => {
                let (a, b) = (a.eval(time), b.eval(time));
                f32::from(match op {
                    '<' => a < b,
                    '>' => a > b,
                    'l' => a <= b,
                    'g' => a >= b,
                    '=' => a == b,
                    '!' => a != b,
                    '&' => a != 0.0 && b != 0.0,
                    _ => a != 0.0 || b != 0.0,
                })
            }
            Expr::If(c, yes, no) => {
                if c.eval(time) != 0.0 {
                    yes.eval(time)
                } else {
                    no.eval(time)
                }
            }
            Expr::Call(f, args) => {
                let a = |i: usize| args.get(i).map_or(0.0, |e| e.eval(time));
                match f {
                    Func::Sin => a(0).to_radians().sin(),
                    Func::Cos => a(0).to_radians().cos(),
                    Func::Abs => a(0).abs(),
                    Func::Min => a(0).min(a(1)),
                    Func::Max => a(0).max(a(1)),
                    Func::Pow => a(0).powf(a(1)),
                    Func::Sqrt => a(0).max(0.0).sqrt(),
                    Func::Floor => a(0).floor(),
                    Func::Ceil => a(0).ceil(),
                    Func::Round => a(0).round(),
                    Func::Trunc => a(0).trunc(),
                    Func::Clamp => a(0).max(a(1)).min(a(2)),
                    Func::Lerp => a(0) + (a(1) - a(0)) * a(2),
                    Func::Mod => {
                        if a(1) == 0.0 {
                            0.0
                        } else {
                            a(0) % a(1)
                        }
                    }
                    Func::Exp => a(0).exp(),
                    Func::Ln => a(0).max(f32::MIN_POSITIVE).ln(),
                }
            }
        };
        if v.is_finite() { v } else { 0.0 }
    }

    pub fn constant(&self) -> Option<f32> {
        match self {
            Expr::Num(n) => Some(*n),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn value(text: &str, time: f32) -> f32 {
        parse(text).unwrap_or_else(|| panic!("{text}")).eval(time)
    }

    #[test]
    fn numbers_and_arithmetic() {
        assert_eq!(value("22.5", 0.0), 22.5);
        assert_eq!(value("-22.5", 0.0), -22.5);
        assert_eq!(value(" 1 + 2 * 3 ", 0.0), 7.0);
        assert_eq!(value("(1 + 2) * 3", 0.0), 9.0);
        assert_eq!(value("10 / 4 - 0.5", 0.0), 2.0);
        assert_eq!(value("1 / 0", 0.0), 0.0);
        assert_eq!(value("", 0.0), 0.0);
        assert_eq!(value("1.5f", 0.0), 1.5);
        assert_eq!(value("- -2", 0.0), 2.0);
    }

    #[test]
    fn functions_work_in_degrees_and_know_the_time() {
        assert!((value("math.sin(90)", 0.0) - 1.0).abs() < 1e-6);
        assert!((value("Math.Sin(query.anim_time * 180) * 10", 0.5) - 10.0).abs() < 1e-4);
        assert!((value("math.cos(q.anim_time*360)", 0.5) + 1.0).abs() < 1e-5);
        assert_eq!(value("math.clamp(5, 0, 2)", 0.0), 2.0);
        assert_eq!(value("math.lerp(0, 10, 0.25)", 0.0), 2.5);
        assert_eq!(value("math.max(1, math.min(7, 3))", 0.0), 3.0);
        assert_eq!(value("math.mod(7, 4)", 0.0), 3.0);
        assert_eq!(value("math.pow(2, 3) + math.sqrt(16)", 0.0), 12.0);
    }

    #[test]
    fn choices() {
        assert_eq!(value("q.anim_time > 0.5 ? 10 : -10", 1.0), 10.0);
        assert_eq!(value("q.anim_time > 0.5 ? 10 : -10", 0.1), -10.0);
        assert_eq!(value("1 < 2 && 2 <= 2", 0.0), 1.0);
        assert_eq!(value("!(1 == 1) || 0", 0.0), 0.0);
        assert_eq!(value("return 3;", 0.0), 3.0);
    }

    #[test]
    fn what_it_does_not_know_is_refused() {
        for text in ["query.is_on_ground", "variable.x + 1", "math.random(0, 1)", "1 +", "(1", "math.sin()", "math.min(1)", "2 3"] {
            assert_eq!(parse(text), None, "{text}");
        }
    }
}

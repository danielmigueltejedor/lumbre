//! `calc.eval`: arithmetic with `+ - * / % ^`, brackets and unary minus, in `f64`.

use serde_json::{json, Value};

use crate::{string_arg, Context, Output, Permission, Tool, ToolError};

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum CalcError {
    #[error("unexpected `{0}`")]
    Unexpected(char),
    #[error("the expression ends too early")]
    Incomplete,
    #[error("division by zero")]
    DivisionByZero,
    #[error("the result is not a finite number")]
    NotFinite,
    #[error("the expression is nested too deeply")]
    TooDeep,
}

struct Parser<'a> {
    chars: std::iter::Peekable<std::str::Chars<'a>>,
    depth: u32,
}

impl Parser<'_> {
    const MAX_DEPTH: u32 = 64;

    fn peek(&mut self) -> Option<char> {
        while self.chars.peek().is_some_and(|ch| ch.is_whitespace()) {
            self.chars.next();
        }
        self.chars.peek().copied()
    }

    fn expr(&mut self) -> Result<f64, CalcError> {
        let mut value = self.term()?;
        while let Some(op @ ('+' | '-')) = self.peek() {
            self.chars.next();
            let rhs = self.term()?;
            value = if op == '+' { value + rhs } else { value - rhs };
        }
        Ok(value)
    }

    fn term(&mut self) -> Result<f64, CalcError> {
        let mut value = self.unary()?;
        while let Some(op @ ('*' | '/' | '%')) = self.peek() {
            self.chars.next();
            let rhs = self.unary()?;
            if op != '*' && rhs == 0.0 {
                return Err(CalcError::DivisionByZero);
            }
            value = match op {
                '*' => value * rhs,
                '/' => value / rhs,
                _ => value % rhs,
            };
        }
        Ok(value)
    }

    fn unary(&mut self) -> Result<f64, CalcError> {
        match self.peek() {
            Some('-') => {
                self.chars.next();
                Ok(-self.unary()?)
            }
            Some('+') => {
                self.chars.next();
                self.unary()
            }
            _ => self.power(),
        }
    }

    fn power(&mut self) -> Result<f64, CalcError> {
        let base = self.atom()?;
        if self.peek() == Some('^') {
            self.chars.next();
            // Right-associative, and binds tighter than unary minus on its left: -2^2 = -4.
            return Ok(base.powf(self.unary()?));
        }
        Ok(base)
    }

    fn atom(&mut self) -> Result<f64, CalcError> {
        match self.peek() {
            Some('(') => {
                self.depth += 1;
                if self.depth > Self::MAX_DEPTH {
                    return Err(CalcError::TooDeep);
                }
                self.chars.next();
                let value = self.expr()?;
                self.depth -= 1;
                match self.peek() {
                    Some(')') => {
                        self.chars.next();
                        Ok(value)
                    }
                    Some(other) => Err(CalcError::Unexpected(other)),
                    None => Err(CalcError::Incomplete),
                }
            }
            Some(ch) if ch.is_ascii_digit() || ch == '.' || ch == ',' => self.number(),
            Some(other) => Err(CalcError::Unexpected(other)),
            None => Err(CalcError::Incomplete),
        }
    }

    fn number(&mut self) -> Result<f64, CalcError> {
        let mut text = String::new();
        while let Some(&ch) = self.chars.peek() {
            match ch {
                '0'..='9' => text.push(ch),
                // Spanish writes the decimal separator as a comma.
                '.' | ',' if !text.contains('.') => text.push('.'),
                _ => break,
            }
            self.chars.next();
        }
        text.parse().map_err(|_| CalcError::Unexpected(text.chars().next().unwrap_or('.')))
    }
}

/// Evaluates an arithmetic expression.
pub fn evaluate(expr: &str) -> Result<f64, CalcError> {
    let mut parser = Parser { chars: expr.chars().peekable(), depth: 0 };
    let value = parser.expr()?;
    if let Some(rest) = parser.peek() {
        return Err(CalcError::Unexpected(rest));
    }
    if !value.is_finite() {
        return Err(CalcError::NotFinite);
    }
    Ok(value)
}

/// Integers print as integers; everything else with up to ten significant digits.
pub fn format_number(value: f64) -> String {
    if value == value.trunc() && value.abs() < 1e15 {
        return format!("{value:.0}");
    }
    let digits = 10 - (value.abs().log10().floor() as i32 + 1).clamp(0, 10);
    let text = format!("{value:.*}", digits.max(0) as usize);
    if text.contains('.') {
        text.trim_end_matches('0').trim_end_matches('.').to_owned()
    } else {
        text
    }
}

pub(crate) struct Calc;

impl Tool for Calc {
    fn name(&self) -> &'static str {
        "calc.eval"
    }

    fn description(&self) -> &'static str {
        "Evaluates an arithmetic expression (+ - * / % ^ and brackets)."
    }

    fn permission(&self) -> Permission {
        Permission::None
    }

    fn invoke(&self, args: &Value, _: &Context) -> Result<Output, ToolError> {
        let expr = string_arg(args, "expr")?;
        let value = evaluate(expr).map_err(|error| ToolError::Failed(error.to_string()))?;
        let result = format_number(value);
        Ok(Output {
            text: format!("{} = {result}", expr.trim()),
            data: json!({ "expr": expr, "value": value }),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn precedence_and_associativity() {
        assert_eq!(evaluate("2+2"), Ok(4.0));
        assert_eq!(evaluate("2 + 3 * 4"), Ok(14.0));
        assert_eq!(evaluate("(2 + 3) * 4"), Ok(20.0));
        assert_eq!(evaluate("2^3^2"), Ok(512.0));
        assert_eq!(evaluate("-2^2"), Ok(-4.0));
        assert_eq!(evaluate("2^-1"), Ok(0.5));
        assert_eq!(evaluate("10 % 4 + 7,5 / 3"), Ok(4.5));
        assert_eq!(evaluate("--3"), Ok(3.0));
    }

    #[test]
    fn errors_are_reported_not_guessed() {
        assert_eq!(evaluate("1/0"), Err(CalcError::DivisionByZero));
        assert_eq!(evaluate("2 +"), Err(CalcError::Incomplete));
        assert_eq!(evaluate("(2"), Err(CalcError::Incomplete));
        assert_eq!(evaluate("2 2"), Err(CalcError::Unexpected('2')));
        assert_eq!(evaluate("2 $ 2"), Err(CalcError::Unexpected('$')));
        assert_eq!(evaluate("10^400"), Err(CalcError::NotFinite));
        assert_eq!(evaluate(&"(".repeat(100)), Err(CalcError::TooDeep));
    }

    #[test]
    fn numbers_print_the_way_a_person_expects() {
        assert_eq!(format_number(4.0), "4");
        assert_eq!(format_number(-84.0), "-84");
        assert_eq!(format_number(2.5), "2.5");
        assert_eq!(format_number(1.0 / 3.0), "0.3333333333");
        assert_eq!(format_number(0.1 + 0.2), "0.3");
        assert_eq!(format_number(12345.678), "12345.678");
    }

    #[test]
    fn the_tool_answers_with_text_and_data() {
        let output = Calc.invoke(&json!({"expr": "12 * (3 + 4)"}), &Context::default()).unwrap();
        assert_eq!(output.text, "12 * (3 + 4) = 84");
        assert_eq!(output.data["value"], 84.0);
    }
}

//! `clock.now`: the local time or date, read from the system clock.

use chrono::{DateTime, Datelike, Local, TimeZone};
use serde_json::{json, Value};

use crate::{Context, Language, Output, Permission, Tool, ToolError};

const DAYS_ES: [&str; 7] = ["lunes", "martes", "miércoles", "jueves", "viernes", "sábado", "domingo"];
const MONTHS_ES: [&str; 12] = [
    "enero",
    "febrero",
    "marzo",
    "abril",
    "mayo",
    "junio",
    "julio",
    "agosto",
    "septiembre",
    "octubre",
    "noviembre",
    "diciembre",
];
const DAYS_EN: [&str; 7] = ["Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday"];
const MONTHS_EN: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

pub(crate) fn describe<Tz: TimeZone>(now: &DateTime<Tz>, what: &str, language: Language) -> String
where
    Tz::Offset: std::fmt::Display,
{
    let weekday = now.weekday().num_days_from_monday() as usize;
    let month = now.month0() as usize;
    match (what, language) {
        ("date", Language::Es) => {
            format!("Hoy es {}, {} de {} de {}.", DAYS_ES[weekday], now.day(), MONTHS_ES[month], now.year())
        }
        ("date", Language::En) => {
            format!("Today is {}, {} {}, {}.", DAYS_EN[weekday], MONTHS_EN[month], now.day(), now.year())
        }
        (_, Language::Es) => format!("Son las {}.", now.format("%H:%M")),
        (_, Language::En) => format!("It is {}.", now.format("%H:%M")),
    }
}

pub(crate) struct Clock;

impl Tool for Clock {
    fn name(&self) -> &'static str {
        "clock.now"
    }

    fn description(&self) -> &'static str {
        "Reads the local time or date from the system clock."
    }

    fn permission(&self) -> Permission {
        Permission::None
    }

    fn invoke(&self, args: &Value, context: &Context) -> Result<Output, ToolError> {
        let what = args.get("what").and_then(Value::as_str).unwrap_or("time");
        if !matches!(what, "time" | "date") {
            return Err(ToolError::Arguments(format!("`what` must be `time` or `date`, not `{what}`")));
        }
        let now = Local::now();
        Ok(Output { text: describe(&now, what, context.language), data: json!({ "iso": now.to_rfc3339() }) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    #[test]
    fn phrasing_in_both_languages() {
        // Wednesday 11 November 2026, 09:05 UTC.
        let moment = Utc.with_ymd_and_hms(2026, 11, 11, 9, 5, 0).unwrap();
        assert_eq!(describe(&moment, "time", Language::Es), "Son las 09:05.");
        assert_eq!(describe(&moment, "time", Language::En), "It is 09:05.");
        assert_eq!(describe(&moment, "date", Language::Es), "Hoy es miércoles, 11 de noviembre de 2026.");
        assert_eq!(describe(&moment, "date", Language::En), "Today is Wednesday, November 11, 2026.");
    }

    #[test]
    fn the_tool_reads_the_real_clock() {
        let before = Local::now();
        let output = Clock.invoke(&json!({"what": "time"}), &Context::default()).unwrap();
        let reported = DateTime::parse_from_rfc3339(output.data["iso"].as_str().unwrap()).unwrap();
        assert!((reported.timestamp() - before.timestamp()).abs() <= 2);
        assert!(Clock.invoke(&json!({"what": "weather"}), &Context::default()).is_err());
    }
}

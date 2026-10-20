//! Lengths of time written the way people write them: `4:10`, `1:02:03.5`,
//! or a bare number of seconds.

use std::fmt;
use std::str::FromStr;
use std::time::Duration;

use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A length of time. Written as `h:mm:ss.fff`, `m:ss`, or plain seconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Hash)]
pub struct Time(Duration);

impl Time {
    /// Zero.
    pub const ZERO: Self = Self(Duration::ZERO);

    /// From seconds.
    #[must_use]
    pub fn secs(s: f64) -> Self {
        Self(Duration::from_secs_f64(s.max(0.0)))
    }

    /// As a [`Duration`].
    #[must_use]
    pub fn duration(self) -> Duration {
        self.0
    }
}

impl From<Duration> for Time {
    fn from(d: Duration) -> Self {
        Self(d)
    }
}

impl From<Time> for Duration {
    fn from(t: Time) -> Self {
        t.0
    }
}

/// A string that is not a time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimeError(String);

impl fmt::Display for TimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "`{}` is not a time; write h:mm:ss, m:ss, or seconds",
            self.0
        )
    }
}

impl std::error::Error for TimeError {}

impl FromStr for Time {
    type Err = TimeError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let err = || TimeError(s.to_string());
        let s = s.trim();
        let parts: Vec<&str> = s.split(':').collect();
        if s.is_empty() || parts.len() > 3 {
            return Err(err());
        }
        let (last, rest) = parts.split_last().ok_or_else(err)?;
        let mut secs: f64 = last.parse().map_err(|_| err())?;
        if !secs.is_finite() || secs < 0.0 {
            return Err(err());
        }
        for (i, part) in rest.iter().rev().enumerate() {
            let n: u32 = part.parse().map_err(|_| err())?;
            let scale = if i == 0 { 60.0 } else { 3600.0 };
            secs += f64::from(n) * scale;
        }
        Ok(Self::secs(secs))
    }
}

impl fmt::Display for Time {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let total_ms = self.0.as_millis();
        let ms = total_ms % 1000;
        let total = total_ms / 1000;
        let (h, m, s) = (total / 3600, total / 60 % 60, total % 60);
        if h > 0 {
            write!(f, "{h}:{m:02}:{s:02}")?;
        } else {
            write!(f, "{m}:{s:02}")?;
        }
        if ms > 0 {
            write!(f, ".{ms:03}")?;
        }
        Ok(())
    }
}

impl Serialize for Time {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Time {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(TimeVisitor)
    }
}

struct TimeVisitor;

impl Visitor<'_> for TimeVisitor {
    type Value = Time;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a time like \"4:10\" or a number of seconds")
    }

    fn visit_str<E: de::Error>(self, v: &str) -> Result<Time, E> {
        v.parse().map_err(de::Error::custom)
    }

    fn visit_f64<E: de::Error>(self, v: f64) -> Result<Time, E> {
        if v.is_finite() && v >= 0.0 {
            Ok(Time::secs(v))
        } else {
            Err(de::Error::custom(TimeError(v.to_string())))
        }
    }

    #[allow(clippy::cast_precision_loss)]
    fn visit_u64<E: de::Error>(self, v: u64) -> Result<Time, E> {
        Ok(Time::secs(v as f64))
    }

    #[allow(clippy::cast_precision_loss)]
    fn visit_i64<E: de::Error>(self, v: i64) -> Result<Time, E> {
        if v < 0 {
            return Err(de::Error::custom(TimeError(v.to_string())));
        }
        Ok(Time::secs(v as f64))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(s: &str) -> Time {
        s.parse().unwrap()
    }

    #[test]
    fn parses_clock_forms() {
        assert_eq!(t("4"), Time::secs(4.0));
        assert_eq!(t("4.5"), Time::secs(4.5));
        assert_eq!(t("4:10"), Time::secs(250.0));
        assert_eq!(t("1:02:03"), Time::secs(3723.0));
        assert_eq!(t("0:00:00.250"), Time::secs(0.25));
        assert_eq!(t(" 12:00 "), Time::secs(720.0));
    }

    #[test]
    fn rejects_junk() {
        for s in ["", "a", "1:2:3:4", "-1", "1:-2", "1:x", "nan", "inf"] {
            assert!(s.parse::<Time>().is_err(), "{s:?} should not parse");
        }
    }

    #[test]
    fn displays_the_shortest_clock_form() {
        assert_eq!(Time::secs(4.0).to_string(), "0:04");
        assert_eq!(Time::secs(250.0).to_string(), "4:10");
        assert_eq!(Time::secs(3723.5).to_string(), "1:02:03.500");
        assert_eq!(Time::secs(36000.0).to_string(), "10:00:00");
    }

    #[test]
    fn round_trips_through_display() {
        for s in ["0:04", "4:10", "1:02:03.500", "10:00:00"] {
            assert_eq!(t(s).to_string(), s);
        }
    }

    #[test]
    fn deserializes_strings_and_numbers() {
        #[derive(Deserialize)]
        struct Doc {
            a: Time,
            b: Time,
            c: Time,
        }
        let doc: Doc = toml::from_str("a = \"4:10\"\nb = 8\nc = 2.5").unwrap();
        assert_eq!(doc.a, Time::secs(250.0));
        assert_eq!(doc.b, Time::secs(8.0));
        assert_eq!(doc.c, Time::secs(2.5));
        assert!(toml::from_str::<Doc>("a = \"x\"\nb = 1\nc = 1").is_err());
        assert!(toml::from_str::<Doc>("a = 1\nb = -1\nc = 1").is_err());
    }
}

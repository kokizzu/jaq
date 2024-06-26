use crate::{ValR2, ValT};
use alloc::string::{String, ToString};
use jaq_interpret::Error;

/// Parse an ISO-8601 timestamp string to a number holding the equivalent UNIX timestamp
/// (seconds elapsed since 1970/01/01).
pub fn from_iso8601<V: ValT>(s: &str) -> ValR2<V> {
    use time::format_description::well_known::Iso8601;
    use time::OffsetDateTime;
    let datetime = OffsetDateTime::parse(s, &Iso8601::DEFAULT)
        .map_err(|e| Error::str(format_args!("cannot parse {s} as ISO-8601 timestamp: {e}")))?;
    let epoch_s = datetime.unix_timestamp();
    if s.contains('.') {
        let seconds = epoch_s as f64 + (f64::from(datetime.nanosecond()) * 1e-9_f64);
        Ok(seconds.into())
    } else {
        isize::try_from(epoch_s)
            .map(V::from)
            .or_else(|_| V::from_num(&epoch_s.to_string()))
    }
}

/// Format a number as an ISO-8601 timestamp string.
pub fn to_iso8601<V: ValT>(v: &V) -> Result<String, Error<V>> {
    use time::format_description::well_known::iso8601;
    use time::OffsetDateTime;
    const SECONDS_CONFIG: iso8601::EncodedConfig = iso8601::Config::DEFAULT
        .set_time_precision(iso8601::TimePrecision::Second {
            decimal_digits: None,
        })
        .encode();

    let fail1 = |e| Error::str(format_args!("cannot format {v} as ISO-8601 timestamp: {e}"));
    let fail2 = |e| Error::str(format_args!("cannot format {v} as ISO-8601 timestamp: {e}"));

    if let Some(i) = v.as_isize() {
        let iso8601_fmt_s = iso8601::Iso8601::<SECONDS_CONFIG>;
        OffsetDateTime::from_unix_timestamp(i as i64)
            .map_err(fail1)?
            .format(&iso8601_fmt_s)
            .map_err(fail2)
    } else {
        let f = v.as_f64()?;
        let f_ns = (f * 1_000_000_000_f64).round() as i128;
        OffsetDateTime::from_unix_timestamp_nanos(f_ns)
            .map_err(fail1)?
            .format(&iso8601::Iso8601::DEFAULT)
            .map_err(fail2)
    }
}

//! ES 21.4 (Date): integral epoch-millisecond instants.
//!
//! The VM has no time-zone database, so every operation is UTC. Local-time
//! forms (`toString`, the component constructor) therefore coincide with
//! their UTC counterparts.
//!
//! TODO(timezone): `TZ` is ignored and `LocalTime`/`UTC` are the identity.
//! ES 21.4.1.24 explicitly allows a UTC-only host, but once tz data is wired
//! up (`GetNamedTimeZoneOffsetNanoseconds`), offsetless `Date.parse` input,
//! the component constructor, `toString`/`toTimeString`, `getTimezoneOffset`
//! and the local getters/setters need to use it.
//!
//! Time values follow ES 21.4.1.1/21.4.1.31: integral milliseconds in the
//! inclusive range ±8.64e15; anything else is NaN. Every entry point funnels
//! through [`time_clip`].

use vm_core::Handle;
use vm_core::HostCtx;
use vm_core::Object;
use vm_core::raise_runtime;
use vm_core::{Args, HandleScope, Heap};
use vm_core::{Coercion, Hint};
use vm_core::{DenseString, MapKind, Tagged, Value, VmError};
use vm_core::{ThreadState, VM};

/// Milliseconds in a UTC day.
const MS_PER_DAY_I64: i64 = 86_400_000;
/// The largest time value (ES 21.4.1.1): 100_000_000 days.
const MAX_TIME_MS: f64 = 8.64e15;
const MAX_TIME_MS_I128: i128 = 8_640_000_000_000_000;
/// A component beyond the largest time value can never reach a valid
/// instant; rejecting it early keeps the civil arithmetic in range.
const MAX_COMPONENT: f64 = MAX_TIME_MS;

const WEEKDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// ES 21.4.1.31 TimeClip: non-finite and out-of-range instants are NaN,
/// everything else is truncated toward zero and normalized to `+0`.
fn time_clip(time: f64) -> f64 {
    if !time.is_finite() || time.abs() > MAX_TIME_MS {
        return f64::NAN;
    }
    time.trunc() + 0.0
}

/// TimeClip for an already-integral instant.
fn time_clip_i128(time: i128) -> f64 {
    if time.abs() > MAX_TIME_MS_I128 {
        f64::NAN
    } else {
        time as f64
    }
}

/// Milliseconds since the Unix epoch, truncated to a whole millisecond.
fn now_millis() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as f64)
        .unwrap_or(0.0)
}

/// Days from 1970-01-01 to (year, month, day).
///
/// `month` is 1-based. `day` may be outside the month's range: the result is
/// the ordinary day count plus `day - 1`, exactly the normalisation ES
/// MakeDay requires (e.g. February 30 → March 1).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// (year, month, day) from days since 1970-01-01.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if month <= 2 { y + 1 } else { y }, month, day)
}

/// Weekday of a day count (0 = Sunday; 1970-01-01 was a Thursday).
fn week_day(days: i64) -> usize {
    (days + 4).rem_euclid(7) as usize
}

/// A broken-down UTC instant.
struct Broken {
    year: i64,
    month: u32,
    day: u32,
    hour: i64,
    min: i64,
    sec: i64,
    ms: i64,
    weekday: usize,
}

/// Break a valid time value down into UTC fields. `None` for NaN and
/// out-of-range values (which cannot arise from TimeClip'd slots).
fn break_down(time: f64) -> Option<Broken> {
    if !time.is_finite() || time.abs() > MAX_TIME_MS {
        return None;
    }
    let time = time.trunc() as i64;
    let days = time.div_euclid(MS_PER_DAY_I64);
    let tod = time.rem_euclid(MS_PER_DAY_I64);
    let (year, month, day) = civil_from_days(days);
    Some(Broken {
        year,
        month,
        day,
        hour: tod / 3_600_000,
        min: (tod / 60_000) % 60,
        sec: (tod / 1_000) % 60,
        ms: tod % 1_000,
        weekday: week_day(days),
    })
}

/// The year in ISO form: 4 digits in 0..=9999, else sign + 6 digits.
fn year_iso(year: i64) -> String {
    if (0..=9999).contains(&year) {
        format!("{year:04}")
    } else if year < 0 {
        format!("-{:06}", year.unsigned_abs())
    } else {
        format!("+{:06}", year.unsigned_abs())
    }
}

/// The year in the legacy Date string forms: at least 4 digits for
/// 0..=9999 (and for negative years, after the sign), else the plain
/// signed number, in the legacy date-string format.
fn year_date_string(year: i64) -> String {
    if year < 0 {
        format!("-{:04}", year.unsigned_abs())
    } else {
        format!("{year:04}")
    }
}

fn format_iso(b: &Broken) -> String {
    format!(
        "{}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        year_iso(b.year),
        b.month,
        b.day,
        b.hour,
        b.min,
        b.sec,
        b.ms
    )
}

fn format_gmt(b: &Broken) -> String {
    format!(
        "{}, {:02} {} {} {:02}:{:02}:{:02} GMT",
        WEEKDAYS[b.weekday],
        b.day,
        MONTHS[(b.month - 1) as usize],
        year_date_string(b.year),
        b.hour,
        b.min,
        b.sec
    )
}

fn format_date_string(b: &Broken) -> String {
    format!(
        "{} {} {:02} {} {:02}:{:02}:{:02} GMT+0000 (Coordinated Universal Time)",
        WEEKDAYS[b.weekday],
        MONTHS[(b.month - 1) as usize],
        b.day,
        year_date_string(b.year),
        b.hour,
        b.min,
        b.sec
    )
}

/// Parse text the way both `Date.parse` and the one-argument constructor do.
fn parse_text(text: &str) -> f64 {
    parse_iso(text)
        .or_else(|| parse_loose(text))
        .unwrap_or(f64::NAN)
}

/// Exactly `len` ASCII digits at `at`, as an integer.
fn fixed_digits(bytes: &[u8], at: usize, len: usize) -> Option<(i64, usize)> {
    let end = at.checked_add(len)?;
    let digits = bytes.get(at..end)?;
    let mut value = 0i64;
    for &byte in digits {
        if !byte.is_ascii_digit() {
            return None;
        }
        value = value * 10 + i64::from(byte - b'0');
    }
    Some((value, end))
}

/// The ECMAScript Date Time String Format (ES 21.4.1.32):
///
/// ```text
/// [±YYYYYY|YYYY][-MM[-DD]][T HH:mm[:ss[.sss]][Z|±HH:mm]]
/// ```
///
/// with the extensions Node also accepts: a space or `t` separator, lowercase
/// `z`, `±HHmm`, and fractional seconds of any length (truncated to
/// milliseconds). Date-only forms are UTC; time forms without an offset are
/// local time, which is UTC here. `None` means the input does not match;
/// `Some(NaN)` means it matched but lies outside the valid time range.
fn parse_iso(text: &str) -> Option<f64> {
    let bytes = text.as_bytes();

    let (year, mut i) = match bytes.first()? {
        b'+' | b'-' => {
            let sign = if bytes[0] == b'-' { -1 } else { 1 };
            let (year, next) = fixed_digits(bytes, 1, 6)?;
            // "-000000" is not a valid expanded year (year zero is positive).
            if sign < 0 && year == 0 {
                return None;
            }
            (sign * year, next)
        }
        byte if byte.is_ascii_digit() => fixed_digits(bytes, 0, 4)?,
        _ => return None,
    };

    let (mut month, mut day) = (1, 1);
    if bytes.get(i) == Some(&b'-') {
        let (value, next) = fixed_digits(bytes, i + 1, 2)?;
        if !(1..=12).contains(&value) {
            return None;
        }
        month = value;
        i = next;
        if bytes.get(i) == Some(&b'-') {
            let (value, next) = fixed_digits(bytes, i + 1, 2)?;
            if !(1..=31).contains(&value) {
                return None;
            }
            day = value;
            i = next;
        }
    }

    let mut offset_minutes = 0i64;
    if matches!(bytes.get(i), Some(b'T' | b't' | b' ')) {
        i += 1;
        let (hour, next) = fixed_digits(bytes, i, 2)?;
        if hour > 24 {
            return None;
        }
        i = next;
        if bytes.get(i) != Some(&b':') {
            return None;
        }
        let (minute, next) = fixed_digits(bytes, i + 1, 2)?;
        if minute > 59 {
            return None;
        }
        i = next;

        let (mut second, mut millis) = (0, 0);
        if bytes.get(i) == Some(&b':') {
            let (value, next) = fixed_digits(bytes, i + 1, 2)?;
            if value > 59 {
                return None;
            }
            second = value;
            i = next;
            if bytes.get(i) == Some(&b'.') {
                i += 1;
                let start = i;
                while bytes.get(i).is_some_and(u8::is_ascii_digit) {
                    i += 1;
                }
                if i == start {
                    return None;
                }
                // The first three fractional digits are the milliseconds;
                // the rest are truncated.
                for k in 0..3 {
                    millis *= 10;
                    if let Some(byte) = bytes.get(start + k).filter(|b| b.is_ascii_digit()) {
                        millis += i64::from(*byte - b'0');
                    }
                }
            }
        }

        // 24:00 is allowed as end-of-day, but nothing later.
        if hour == 24 && (minute != 0 || second != 0 || millis != 0) {
            return None;
        }

        match bytes.get(i) {
            Some(b'Z' | b'z') => i += 1,
            Some(sign @ (b'+' | b'-')) => {
                let sign = if *sign == b'-' { -1 } else { 1 };
                i += 1;
                // Node also accepts the four-digit `±HHmm` extension.
                if let Some((value, next)) = fixed_digits(bytes, i, 4) {
                    let (hour, minute) = (value / 100, value % 100);
                    if hour > 23 || minute > 59 {
                        return None;
                    }
                    offset_minutes = sign * (hour * 60 + minute);
                    i = next;
                } else {
                    let (hour, next) = fixed_digits(bytes, i, 2)?;
                    if hour > 23 {
                        return None;
                    }
                    i = next;
                    if bytes.get(i) != Some(&b':') {
                        return None;
                    }
                    let (minute, next) = fixed_digits(bytes, i + 1, 2)?;
                    if minute > 59 {
                        return None;
                    }
                    offset_minutes = sign * (hour * 60 + minute);
                    i = next;
                }
            }
            None => {}
            _ => return None,
        }

        if i != bytes.len() {
            return None;
        }
        let time_of_day = ((hour * 60 + minute) * 60 + second) * 1_000 + millis;
        let days = i128::from(days_from_civil(year, month, day));
        let total = days * i128::from(MS_PER_DAY_I64) + i128::from(time_of_day)
            - i128::from(offset_minutes) * 60_000;
        return Some(time_clip_i128(total));
    }

    if i != bytes.len() {
        return None;
    }

    // Date-only form: always UTC.
    let days = i128::from(days_from_civil(year, month, day));
    Some(time_clip_i128(days * i128::from(MS_PER_DAY_I64)))
}

/// The tokenized forms we emit ourselves: `"Thu, 01 Jan 1970 00:00:00 GMT"`
/// and `"Thu Jan 01 1970 00:00:00 GMT+0000 (...)"`.
fn parse_loose(text: &str) -> Option<f64> {
    let (mut month, mut day, mut year) = (None, None, None);
    let (mut hour, mut minute, mut second) = (0i64, 0i64, 0i64);
    for token in text.split(|c: char| c.is_whitespace() || matches!(c, ',' | '(' | ')')) {
        if token.is_empty() || token.starts_with("GMT") || token.starts_with("gmt") {
            continue;
        }
        if month.is_none()
            && let Some(m) = MONTHS.iter().position(|m| m.eq_ignore_ascii_case(token))
        {
            month = Some(m as i64 + 1);
            continue;
        }
        if token.contains(':') {
            let mut it = token.split(':');
            hour = it.next()?.parse().ok()?;
            minute = it.next().and_then(|s| s.parse().ok())?;
            second = it.next().and_then(|s| s.parse().ok()).unwrap_or(0);
            continue;
        }
        if let Ok(n) = token.parse::<i64>() {
            // 1-2 digit non-negative token is the day of month, the rest
            // is the year (3+ digits, or signed)
            if token.len() < 3 && !token.starts_with(['+', '-']) && day.is_none() {
                day = Some(n);
            } else if year.is_none() {
                year = Some(n);
            }
            continue;
        }
    }
    let (month, day, year) = (month?, day?, year?);
    if !(1..=31).contains(&day) || hour > 24 || minute > 59 || second > 59 {
        return None;
    }
    // Anything beyond the valid year range would clip to NaN anyway; the
    // bound also keeps `days_from_civil` free of overflow.
    if year.abs() > 1_000_000 {
        return None;
    }
    let days = i128::from(days_from_civil(year, month, day));
    let time = i128::from((hour * 60 + minute) * 60 + second) * 1_000;
    Some(time_clip_i128(days * i128::from(MS_PER_DAY_I64) + time))
}

/// `Date.parse(string)` (ES 21.4.3.2): the ISO form plus the formats our
/// `toGMTString`/`toString` emit. Unparseable input is NaN.
pub fn date_parse<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx { vm, heap, state } = ctx;
    let arg = args.get_handle(heap, 1);
    // Fast path: a primitive string needs no ToString coercion.
    if let Some(text) = arg.as_tagged(heap).get_as::<DenseString>(heap) {
        let text = text.to_rust_string(heap);
        return heap.new_number(parse_text(&text));
    }
    state.handle_scope(|scope| {
        let text = match Object::to_string(vm, heap, state, arg) {
            Ok(Some(s)) => {
                let s = scope.handle(s);
                s.as_tagged(heap)
                    .get_as::<DenseString>(heap)
                    .map(|d| d.to_rust_string(heap))
                    .unwrap_or_default()
            }
            Ok(None) => return vm.known().exception.get(heap).erase(),
            Err(err) => return raise_runtime(vm, heap, state, err),
        };
        heap.new_number(parse_text(&text))
    })
}

/// One-argument `new Date(value)`: a Date argument contributes its
/// [[DateValue]] directly; otherwise ToPrimitive with the default hint,
/// then parse a String or TimeClip a Number (ES 21.4.2.1).
fn date_from_value(
    vm: &VM,
    heap: &mut Heap,
    state: &ThreadState,
    value: Handle<'_, Value>,
) -> Result<Option<f64>, VmError> {
    let tagged = value.as_tagged(heap);
    // Fast paths: numbers, strings, and the other primitives need neither
    // ToPrimitive nor a handle scope.
    if let Some(number) = tagged.as_number(heap) {
        return Ok(Some(time_clip(number)));
    }
    if let Some(text) = tagged.get_as::<DenseString>(heap) {
        return Ok(Some(parse_text(&text.to_rust_string(heap))));
    }
    // A Date argument carries its instant in [[DateValue]]; ES reads that
    // slot instead of running ToPrimitive (which could call user code).
    if let Some(millis) = date_value(heap, tagged) {
        return Ok(Some(time_clip(millis)));
    }
    if tagged.is_primitive(heap) {
        return Ok(Some(time_clip(tagged.to_number(heap)?)));
    }
    state.handle_scope(
        |scope| match Object::to_primitive(vm, heap, state, value, Hint::Default)? {
            Coercion::Threw => Ok(None),
            Coercion::Value(value) => {
                let value = scope.handle(value);
                if let Some(text) = value.as_tagged(heap).get_as::<DenseString>(heap) {
                    let text = text.to_rust_string(heap);
                    Ok(Some(parse_text(&text)))
                } else {
                    let number = value.as_tagged(heap).to_number(heap)?;
                    Ok(Some(time_clip(number)))
                }
            }
        },
    )
}

/// `MakeDay`/`MakeTime`/`MakeDate` plus `TimeClip` on the seven components
/// (`UTC` is the identity here). Non-finite or absurdly large fields give NaN,
/// as do instants outside the valid range.
fn make_time_value(parts: [f64; 7]) -> f64 {
    if parts
        .iter()
        .any(|part| !part.is_finite() || part.abs() > MAX_COMPONENT)
    {
        return f64::NAN;
    }
    let [year, month, date, hours, minutes, seconds, millis] = parts.map(f64::trunc);
    let month = month as i64;
    let date = date as i64;
    // MakeDay: normalise the month into a year offset and a 1-based month.
    let year = year as i64 + month.div_euclid(12);
    let month = month.rem_euclid(12) + 1;
    let days = i128::from(days_from_civil(year, month, 1)) + i128::from(date) - 1;
    let time = i128::from(hours as i64) * 3_600_000
        + i128::from(minutes as i64) * 60_000
        + i128::from(seconds as i64) * 1_000
        + i128::from(millis as i64);
    time_clip_i128(days * i128::from(MS_PER_DAY_I64) + time)
}

/// `MakeFullYear ( year )`: a year in 0..=99 means 1900 + year.
fn make_full_year(year: f64) -> f64 {
    if year.is_nan() {
        return f64::NAN;
    }
    let year = year.trunc();
    if (0.0..=99.0).contains(&year) {
        year + 1900.0
    } else {
        year
    }
}

/// `ToNumber` for a Date argument: primitives convert directly, objects go
/// through `ToPrimitive` and may run user code.
fn component_number(
    vm: &VM,
    heap: &mut Heap,
    state: &ThreadState,
    value: Handle<'_, Value>,
) -> Result<Option<f64>, VmError> {
    if value.as_tagged(heap).is_primitive(heap) {
        return Ok(Some(value.as_tagged(heap).to_number(heap)?));
    }
    Object::to_numeric(vm, heap, state, value)
}

fn date_from_components(
    vm: &VM,
    heap: &mut Heap,
    state: &ThreadState,
    args: Args,
) -> Result<Option<f64>, VmError> {
    // year, month, date=1, hours=0, minutes=0, seconds=0, ms=0
    let mut parts = [0.0f64; 7];
    parts[2] = 1.0;
    let count = args.len().saturating_sub(1).min(parts.len());
    for (index, part) in parts.iter_mut().enumerate().take(count) {
        let value = args.get_handle(heap, index + 1);
        match component_number(vm, heap, state, value)? {
            Some(value) => *part = value,
            None => return Ok(None),
        }
    }
    parts[0] = make_full_year(parts[0]);
    Ok(Some(make_time_value(parts)))
}

/// `Date ( ...values )`
/// [ES 21.4.2.1](https://tc39.es/ecma262/#sec-date)
/// Called, returns `ToDateString(now)`; constructed, wraps the time value.
pub fn date_constructor<'a>(
    ctx: HostCtx<'a>,
    new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx { vm, heap, state } = ctx;
    // The call form ignores every argument.
    let Some(new_target) = new_target else {
        return state.handle_scope(|scope| {
            let text = break_down(time_clip(now_millis()))
                .map(|broken| format_date_string(&broken))
                .unwrap_or_else(|| "Invalid Date".to_string());
            DenseString::from_utf8(heap, &scope, &text)
                .as_tagged(heap)
                .erase()
        });
    };
    let time = if args.len() < 2 {
        time_clip(now_millis())
    } else if args.len() == 2 {
        let value = args.get_handle(heap, 1);
        match date_from_value(vm, heap, state, value) {
            Ok(Some(time)) => time,
            Ok(None) => return vm.known().exception.get(heap).erase(),
            Err(err) => return raise_runtime(vm, heap, state, err),
        }
    } else {
        match date_from_components(vm, heap, state, args) {
            Ok(Some(time)) => time,
            Ok(None) => return vm.known().exception.get(heap).erase(),
            Err(err) => return raise_runtime(vm, heap, state, err),
        }
    };
    state.handle_scope(|scope| {
        let map = scope.handle(vm.known().date_instance_map.get(heap));
        let time = scope.handle(heap.new_number(time));
        let slots = scope.stage(&[time.as_tagged(heap)]);
        let object = match Object::create_from_constructor(
            vm, heap, state, &scope, new_target, map, slots,
        ) {
            Ok(Some(object)) => object,
            Ok(None) => return vm.known().exception.get(heap).erase(),
            Err(err) => return raise_runtime(vm, heap, state, err),
        };
        object.as_tagged(heap).erase()
    })
}

/// `Date.now ( )`
/// [ES 21.4.3.1](https://tc39.es/ecma262/#sec-date.now)
/// Returns the current time value in epoch milliseconds.
pub fn date_now<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    _args: Args,
) -> Tagged<'a, Value> {
    ctx.heap.new_number(time_clip(now_millis()))
}

/// The [[DateValue]] of a receiver, or `None` when it is not a Date.
fn date_value(heap: &Heap, receiver: Tagged<'_, Value>) -> Option<f64> {
    let object = receiver.as_heap_object()?;
    if !object.as_ref().map(heap).kind().contains(MapKind::DATE) {
        return None;
    }
    object.as_ref().slots(heap).at(heap, 0).as_number(heap)
}

/// `Date.prototype.getTime` (ES 21.4.4.9): the wrapped time value.
pub fn date_get_time<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx { vm, heap, state } = ctx;
    let Some(millis) = date_value(heap, args.get(heap, 0)) else {
        return raise_runtime(vm, heap, state, VmError::Type);
    };
    heap.new_number(millis)
}

/// `Date.prototype.valueOf` (ES 21.4.4.40): the same [[DateValue]] access
/// as `getTime`.
pub fn date_value_of<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    date_get_time(ctx, None, args)
}

/// A `Date.prototype.get*` component (ES 21.4.4.2-21.4.4.9/21.4.4.12-21.4.4.19).
#[derive(Copy, Clone)]
enum Field {
    Year,
    Month,
    Date,
    WeekDay,
    Hours,
    Minutes,
    Seconds,
    Milliseconds,
}

impl Field {
    /// The component of a valid instant, as a Number.
    fn read(self, broken: &Broken) -> f64 {
        match self {
            Field::Year => broken.year as f64,
            Field::Month => f64::from(broken.month - 1),
            Field::Date => f64::from(broken.day),
            Field::WeekDay => broken.weekday as f64,
            Field::Hours => broken.hour as f64,
            Field::Minutes => broken.min as f64,
            Field::Seconds => broken.sec as f64,
            Field::Milliseconds => broken.ms as f64,
        }
    }
}

/// Shared body of the getters: `𝔽(op(LocalTime(tv)))`, NaN for an invalid
/// Date. `LocalTime` is the identity, so the local and UTC forms coincide.
fn get_field<'a>(ctx: HostCtx<'a>, args: Args, field: Field) -> Tagged<'a, Value> {
    let HostCtx { vm, heap, state } = ctx;
    let Some(time) = date_value(heap, args.get(heap, 0)) else {
        return raise_runtime(vm, heap, state, VmError::Type);
    };
    match break_down(time) {
        Some(broken) => heap.new_number(field.read(&broken)),
        None => heap.new_number(f64::NAN),
    }
}

/// The components a `Date.prototype.set*` method replaces: the first field
/// index and how many arguments it consumes.
#[derive(Copy, Clone)]
enum SetSpan {
    Milliseconds,
    Seconds,
    Minutes,
    Hours,
    Date,
    Month,
    Year,
}

impl SetSpan {
    /// `year, month, date, hours, minutes, seconds, ms`
    fn first(self) -> usize {
        match self {
            SetSpan::Year => 0,
            SetSpan::Month => 1,
            SetSpan::Date => 2,
            SetSpan::Hours => 3,
            SetSpan::Minutes => 4,
            SetSpan::Seconds => 5,
            SetSpan::Milliseconds => 6,
        }
    }

    fn count(self) -> usize {
        match self {
            SetSpan::Year => 3,
            SetSpan::Month => 2,
            SetSpan::Date => 1,
            SetSpan::Hours => 4,
            SetSpan::Minutes => 3,
            SetSpan::Seconds => 2,
            SetSpan::Milliseconds => 1,
        }
    }
}

/// Write `time` into the receiver's `[[DateValue]]` and return it.
fn store_time<'a>(
    heap: &'a mut Heap,
    scope: &HandleScope<'_>,
    receiver: Handle<'_, Value>,
    time: f64,
) -> Tagged<'a, Value> {
    let number = scope.handle(heap.new_number(time));
    let object = receiver
        .as_tagged(heap)
        .as_heap_object()
        .expect("a Date receiver is an object");
    object
        .as_ref()
        .slot(heap, 0)
        .set(heap, object.erase(), number.as_tagged(heap));
    number.as_tagged(heap).erase()
}

/// Shared body of the component setters (ES 21.4.4.20-21.4.4.26): ToNumber the
/// supplied components in order, keep the receiver's other fields, rebuild the
/// instant and write it back. `invalid_fills_zero` is the `setFullYear` rule
/// that recovers an invalid Date instead of returning NaN.
fn set_fields<'a>(
    ctx: HostCtx<'a>,
    args: Args,
    span: SetSpan,
    invalid_fills_zero: bool,
) -> Tagged<'a, Value> {
    let HostCtx { vm, heap, state } = ctx;
    let receiver = args.get_handle(heap, 0);
    let Some(current) = date_value(heap, receiver.as_tagged(heap)) else {
        return raise_runtime(vm, heap, state, VmError::Type);
    };
    let first = span.first();
    // The setter's first component is mandatory in the signature: a missing
    // argument is `undefined`, so ToNumber turns it into NaN.
    let supplied = args.len().saturating_sub(1).max(1).min(span.count());
    let mut parts = [0.0f64; 7];
    for (offset, part) in parts[first..first + supplied].iter_mut().enumerate() {
        match component_number(vm, heap, state, args.get_handle(heap, offset + 1)) {
            Ok(Some(value)) => *part = value,
            Ok(None) => return vm.known().exception.get(heap).erase(),
            Err(err) => return raise_runtime(vm, heap, state, err),
        }
    }
    let current = if current.is_nan() {
        if !invalid_fills_zero {
            return heap.new_number(f64::NAN);
        }
        0.0
    } else {
        current
    };
    let Some(broken) = break_down(current) else {
        return raise_runtime(vm, heap, state, VmError::Type);
    };
    let existing = [
        broken.year as f64,
        f64::from(broken.month - 1),
        f64::from(broken.day),
        broken.hour as f64,
        broken.min as f64,
        broken.sec as f64,
        broken.ms as f64,
    ];
    parts[..first].copy_from_slice(&existing[..first]);
    parts[first + supplied..].copy_from_slice(&existing[first + supplied..]);
    let time = make_time_value(parts);
    state.handle_scope(|scope| store_time(heap, &scope, receiver, time))
}

/// `Date.prototype.setTime ( time )`
/// [ES 21.4.4.27](https://tc39.es/ecma262/#sec-date.prototype.settime)
/// Sets the time value to `TimeClip(ToNumber(time))`.
pub fn date_set_time<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx { vm, heap, state } = ctx;
    let receiver = args.get_handle(heap, 0);
    if date_value(heap, receiver.as_tagged(heap)).is_none() {
        return raise_runtime(vm, heap, state, VmError::Type);
    }
    let time = match component_number(vm, heap, state, args.get_handle(heap, 1)) {
        Ok(Some(time)) => time_clip(time),
        Ok(None) => return vm.known().exception.get(heap).erase(),
        Err(err) => return raise_runtime(vm, heap, state, err),
    };
    state.handle_scope(|scope| store_time(heap, &scope, receiver, time))
}

/// `Date.prototype.getTimezoneOffset ( )`
/// [ES 21.4.4.11](https://tc39.es/ecma262/#sec-date.prototype.gettimezoneoffset)
/// Returns the UTC offset in minutes, always +0 on this UTC-only host.
pub fn date_get_timezone_offset<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx { vm, heap, state } = ctx;
    let Some(time) = date_value(heap, args.get(heap, 0)) else {
        return raise_runtime(vm, heap, state, VmError::Type);
    };
    // `(tv - LocalTime(tv)) / msPerMinute` with `LocalTime` the identity.
    heap.new_number(if time.is_nan() { f64::NAN } else { 0.0 })
}

/// `Date.prototype.getYear ( )`
/// [ES B.2.3.1](https://tc39.es/ecma262/#sec-date.prototype.getyear)
/// Returns the year minus 1900 (legacy).
pub fn date_get_year<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx { vm, heap, state } = ctx;
    let Some(time) = date_value(heap, args.get(heap, 0)) else {
        return raise_runtime(vm, heap, state, VmError::Type);
    };
    match break_down(time) {
        Some(broken) => heap.new_number(broken.year as f64 - 1900.0),
        None => heap.new_number(f64::NAN),
    }
}

/// `Date.prototype.getDate ( )`
/// [ES 21.4.4.2](https://tc39.es/ecma262/#sec-date.prototype.getdate)
/// Returns the day of the month (1..31).
pub fn date_get_date<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    get_field(ctx, args, Field::Date)
}

/// `Date.prototype.getDay ( )`
/// [ES 21.4.4.3](https://tc39.es/ecma262/#sec-date.prototype.getday)
/// Returns the weekday (0 = Sunday).
pub fn date_get_day<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    get_field(ctx, args, Field::WeekDay)
}

/// `Date.prototype.getFullYear ( )`
/// [ES 21.4.4.4](https://tc39.es/ecma262/#sec-date.prototype.getfullyear)
/// Returns the full year.
pub fn date_get_full_year<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    get_field(ctx, args, Field::Year)
}

/// `Date.prototype.getHours ( )`
/// [ES 21.4.4.5](https://tc39.es/ecma262/#sec-date.prototype.gethours)
/// Returns the hour (0..23).
pub fn date_get_hours<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    get_field(ctx, args, Field::Hours)
}

/// `Date.prototype.getMilliseconds ( )`
/// [ES 21.4.4.6](https://tc39.es/ecma262/#sec-date.prototype.getmilliseconds)
/// Returns the millisecond (0..999).
pub fn date_get_milliseconds<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    get_field(ctx, args, Field::Milliseconds)
}

/// `Date.prototype.getMinutes ( )`
/// [ES 21.4.4.7](https://tc39.es/ecma262/#sec-date.prototype.getminutes)
/// Returns the minute (0..59).
pub fn date_get_minutes<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    get_field(ctx, args, Field::Minutes)
}

/// `Date.prototype.getMonth ( )`
/// [ES 21.4.4.8](https://tc39.es/ecma262/#sec-date.prototype.getmonth)
/// Returns the month index (0..11).
pub fn date_get_month<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    get_field(ctx, args, Field::Month)
}

/// `Date.prototype.getSeconds ( )`
/// [ES 21.4.4.9](https://tc39.es/ecma262/#sec-date.prototype.getseconds)
/// Returns the second (0..59).
pub fn date_get_seconds<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    get_field(ctx, args, Field::Seconds)
}

/// `Date.prototype.getUTCDate ( )`
/// [ES 21.4.4.12](https://tc39.es/ecma262/#sec-date.prototype.getutcdate)
/// Returns the UTC day of the month (1..31).
pub fn date_get_utc_date<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    get_field(ctx, args, Field::Date)
}

/// `Date.prototype.getUTCDay ( )`
/// [ES 21.4.4.13](https://tc39.es/ecma262/#sec-date.prototype.getutcday)
/// Returns the UTC weekday (0 = Sunday).
pub fn date_get_utc_day<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    get_field(ctx, args, Field::WeekDay)
}

/// `Date.prototype.getUTCFullYear ( )`
/// [ES 21.4.4.14](https://tc39.es/ecma262/#sec-date.prototype.getutcfullyear)
/// Returns the UTC full year.
pub fn date_get_utc_full_year<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    get_field(ctx, args, Field::Year)
}

/// `Date.prototype.getUTCHours ( )`
/// [ES 21.4.4.15](https://tc39.es/ecma262/#sec-date.prototype.getutchours)
/// Returns the UTC hour (0..23).
pub fn date_get_utc_hours<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    get_field(ctx, args, Field::Hours)
}

/// `Date.prototype.getUTCMilliseconds ( )`
/// [ES 21.4.4.16](https://tc39.es/ecma262/#sec-date.prototype.getutcmilliseconds)
/// Returns the UTC millisecond (0..999).
pub fn date_get_utc_milliseconds<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    get_field(ctx, args, Field::Milliseconds)
}

/// `Date.prototype.getUTCMinutes ( )`
/// [ES 21.4.4.17](https://tc39.es/ecma262/#sec-date.prototype.getutcminutes)
/// Returns the UTC minute (0..59).
pub fn date_get_utc_minutes<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    get_field(ctx, args, Field::Minutes)
}

/// `Date.prototype.getUTCMonth ( )`
/// [ES 21.4.4.18](https://tc39.es/ecma262/#sec-date.prototype.getutcmonth)
/// Returns the UTC month index (0..11).
pub fn date_get_utc_month<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    get_field(ctx, args, Field::Month)
}

/// `Date.prototype.getUTCSeconds ( )`
/// [ES 21.4.4.19](https://tc39.es/ecma262/#sec-date.prototype.getutcseconds)
/// Returns the UTC second (0..59).
pub fn date_get_utc_seconds<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    get_field(ctx, args, Field::Seconds)
}

/// `Date.prototype.setDate ( ... )`
/// [ES 21.4.4.20](https://tc39.es/ecma262/#sec-date.prototype.setdate)
/// Sets the the day of the month and returns the new time value.
pub fn date_set_date<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    set_fields(ctx, args, SetSpan::Date, false)
}

/// `Date.prototype.setFullYear ( ... )`
/// [ES 21.4.4.21](https://tc39.es/ecma262/#sec-date.prototype.setfullyear)
/// Sets the the year (and month/day) and returns the new time value.
pub fn date_set_full_year<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    set_fields(ctx, args, SetSpan::Year, true)
}

/// `Date.prototype.setHours ( ... )`
/// [ES 21.4.4.22](https://tc39.es/ecma262/#sec-date.prototype.sethours)
/// Sets the the hour (and minute/second/ms) and returns the new time value.
pub fn date_set_hours<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    set_fields(ctx, args, SetSpan::Hours, false)
}

/// `Date.prototype.setMilliseconds ( ... )`
/// [ES 21.4.4.23](https://tc39.es/ecma262/#sec-date.prototype.setmilliseconds)
/// Sets the the millisecond and returns the new time value.
pub fn date_set_milliseconds<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    set_fields(ctx, args, SetSpan::Milliseconds, false)
}

/// `Date.prototype.setMinutes ( ... )`
/// [ES 21.4.4.24](https://tc39.es/ecma262/#sec-date.prototype.setminutes)
/// Sets the the minute (and second/ms) and returns the new time value.
pub fn date_set_minutes<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    set_fields(ctx, args, SetSpan::Minutes, false)
}

/// `Date.prototype.setMonth ( ... )`
/// [ES 21.4.4.25](https://tc39.es/ecma262/#sec-date.prototype.setmonth)
/// Sets the the month (and day) and returns the new time value.
pub fn date_set_month<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    set_fields(ctx, args, SetSpan::Month, false)
}

/// `Date.prototype.setSeconds ( ... )`
/// [ES 21.4.4.26](https://tc39.es/ecma262/#sec-date.prototype.setseconds)
/// Sets the the second (and ms) and returns the new time value.
pub fn date_set_seconds<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    set_fields(ctx, args, SetSpan::Seconds, false)
}

/// `Date.prototype.setUTCDate ( ... )`
/// [ES 21.4.4.28](https://tc39.es/ecma262/#sec-date.prototype.setutcdate)
/// Sets the UTC the day of the month and returns the new time value.
pub fn date_set_utc_date<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    set_fields(ctx, args, SetSpan::Date, false)
}

/// `Date.prototype.setUTCFullYear ( ... )`
/// [ES 21.4.4.29](https://tc39.es/ecma262/#sec-date.prototype.setutcfullyear)
/// Sets the UTC the year (and month/day) and returns the new time value.
pub fn date_set_utc_full_year<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    set_fields(ctx, args, SetSpan::Year, true)
}

/// `Date.prototype.setUTCHours ( ... )`
/// [ES 21.4.4.30](https://tc39.es/ecma262/#sec-date.prototype.setutchours)
/// Sets the UTC the hour (and minute/second/ms) and returns the new time value.
pub fn date_set_utc_hours<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    set_fields(ctx, args, SetSpan::Hours, false)
}

/// `Date.prototype.setUTCMilliseconds ( ... )`
/// [ES 21.4.4.31](https://tc39.es/ecma262/#sec-date.prototype.setutcmilliseconds)
/// Sets the UTC the millisecond and returns the new time value.
pub fn date_set_utc_milliseconds<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    set_fields(ctx, args, SetSpan::Milliseconds, false)
}

/// `Date.prototype.setUTCMinutes ( ... )`
/// [ES 21.4.4.32](https://tc39.es/ecma262/#sec-date.prototype.setutcminutes)
/// Sets the UTC the minute (and second/ms) and returns the new time value.
pub fn date_set_utc_minutes<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    set_fields(ctx, args, SetSpan::Minutes, false)
}

/// `Date.prototype.setUTCMonth ( ... )`
/// [ES 21.4.4.33](https://tc39.es/ecma262/#sec-date.prototype.setutcmonth)
/// Sets the UTC the month (and day) and returns the new time value.
pub fn date_set_utc_month<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    set_fields(ctx, args, SetSpan::Month, false)
}

/// `Date.prototype.setUTCSeconds ( ... )`
/// [ES 21.4.4.34](https://tc39.es/ecma262/#sec-date.prototype.setutcseconds)
/// Sets the UTC the second (and ms) and returns the new time value.
pub fn date_set_utc_seconds<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    set_fields(ctx, args, SetSpan::Seconds, false)
}

/// `Date.prototype.setYear ( year )`
/// [ES B.2.3.2](https://tc39.es/ecma262/#sec-date.prototype.setyear)
/// Sets the legacy year (0..99 means 19xx) and returns the new time value.
pub fn date_set_year<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx { vm, heap, state } = ctx;
    let receiver = args.get_handle(heap, 0);
    let Some(current) = date_value(heap, receiver.as_tagged(heap)) else {
        return raise_runtime(vm, heap, state, VmError::Type);
    };
    let year = match component_number(vm, heap, state, args.get_handle(heap, 1)) {
        Ok(Some(year)) => year,
        Ok(None) => return vm.known().exception.get(heap).erase(),
        Err(err) => return raise_runtime(vm, heap, state, err),
    };
    let current = if current.is_nan() { 0.0 } else { current };
    let Some(broken) = break_down(current) else {
        return raise_runtime(vm, heap, state, VmError::Type);
    };
    let time = make_time_value([
        make_full_year(year),
        f64::from(broken.month - 1),
        f64::from(broken.day),
        broken.hour as f64,
        broken.min as f64,
        broken.sec as f64,
        broken.ms as f64,
    ]);
    state.handle_scope(|scope| store_time(heap, &scope, receiver, time))
}

/// Shared body of the string formatters: read the receiver's time value and
/// format it, or emit `invalid` for an invalid Date.
fn format_with<'a>(
    ctx: HostCtx<'a>,
    args: Args,
    format: fn(&Broken) -> String,
    invalid: &'static str,
) -> Tagged<'a, Value> {
    let HostCtx { vm, heap, state } = ctx;
    let Some(millis) = date_value(heap, args.get(heap, 0)) else {
        return raise_runtime(vm, heap, state, VmError::Type);
    };
    let text = match break_down(millis) {
        Some(broken) => format(&broken),
        None => invalid.to_string(),
    };
    state.handle_scope(|scope| {
        DenseString::from_utf8(heap, &scope, &text)
            .as_tagged(heap)
            .erase()
    })
}

/// `Date.prototype.toISOString` (ES 21.4.4.36): the ISO 8601 extended form.
/// An invalid Date is a RangeError.
pub fn date_to_iso_string<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx { vm, heap, state } = ctx;
    let Some(millis) = date_value(heap, args.get(heap, 0)) else {
        return raise_runtime(vm, heap, state, VmError::Type);
    };
    let Some(broken) = break_down(millis) else {
        return raise_runtime(vm, heap, state, VmError::InvalidTime);
    };
    state.handle_scope(|scope| {
        let text = format_iso(&broken);
        DenseString::from_utf8(heap, &scope, &text)
            .as_tagged(heap)
            .erase()
    })
}

/// `Date.prototype.toGMTString` / `toUTCString` (ES 21.4.4.43).
pub fn date_to_gmt_string<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    format_with(ctx, args, format_gmt, "Invalid Date")
}

/// `Date.prototype.toString` (ES 21.4.4.41; UTC-formatted).
pub fn date_to_string<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    format_with(ctx, args, format_date_string, "Invalid Date")
}

/// `Date.prototype.toJSON` (ES 21.4.4.37): `null` for non-finite time
/// values, otherwise the result of invoking `this.toISOString()`.
pub fn date_to_json<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx { vm, heap, state } = ctx;
    state.handle_scope(|scope| {
        let this = args.get_handle(heap, 0);
        let object = scope.handle(this.as_tagged(heap));
        let primitive = match Object::to_primitive(vm, heap, state, object, Hint::Number) {
            Ok(Coercion::Value(value)) => scope.handle(value),
            Ok(Coercion::Threw) => return vm.known().exception.get(heap).erase(),
            Err(err) => return raise_runtime(vm, heap, state, err),
        };
        if primitive
            .as_tagged(heap)
            .as_number(heap)
            .is_some_and(|number| !number.is_finite())
        {
            return vm.known().null.get(heap).erase();
        }
        let name = scope.handle(vm.known().strings.to_iso_string.get(heap).erase());
        let method = match Object::get_property(vm, heap, state, object, object, name, false) {
            Ok(Coercion::Value(value)) => scope.handle(value),
            Ok(Coercion::Threw) => return vm.known().exception.get(heap).erase(),
            Err(err) => return raise_runtime(vm, heap, state, err),
        };
        if !method.as_tagged(heap).is_callable(heap) {
            return raise_runtime(vm, heap, state, VmError::Type);
        }
        let call_args = scope.stage(&[object.as_tagged(heap).erase()]);
        // Root the call result before dropping the heap borrow it carries:
        // that keeps the error path free of a second `&mut Heap` while the
        // first is still anchored.
        let outcome = match HostCtx::enter(vm, heap, state, method, call_args, None) {
            Ok(value) => Ok(scope.handle(value)),
            Err(err) => Err(err),
        };
        match outcome {
            Ok(value) => value.as_tagged(heap).erase(),
            Err(err) => raise_runtime(vm, heap, state, err),
        }
    })
}

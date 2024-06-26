//! Core filters.
#![no_std]
#![forbid(unsafe_code)]
#![warn(missing_docs)]

extern crate alloc;
#[cfg(feature = "std")]
extern crate std;

#[cfg(feature = "math")]
mod math;
#[cfg(feature = "regex")]
mod regex;
#[cfg(feature = "time")]
mod time;

use alloc::string::{String, ToString};
use alloc::{borrow::ToOwned, boxed::Box, rc::Rc, vec::Vec};
use jaq_interpret::error::{self, Error};
use jaq_interpret::results::{run_if_ok, then};
use jaq_interpret::{Args, FilterT, Native, RunPtr, UpdatePtr, Val, ValR};

type BoxIter<'a, T> = Box<dyn Iterator<Item = T> + 'a>;
type ValR2<V> = Result<V, Error<V>>;
type ValR2s<'a, V> = BoxIter<'a, ValR2<V>>;

type Filter<V = Val> = (String, usize, Native<V>);

/// Return the minimal set of named filters available in jaq
/// which are implemented as native filters, such as `length`, `keys`, ...,
/// but not `now`, `debug`, `fromdateiso8601`, ...
///
/// Does not return filters from the standard library, such as `map`.
pub fn minimal() -> impl Iterator<Item = Filter> {
    run_many(JSON_MINIMAL.into()).chain(generic_base())
}

/// Minimal set of filters that are generic over the value type.
pub fn generic_base<V: ValT>() -> impl Iterator<Item = Filter<V>> {
    run_many(core_run()).chain([upd(error())])
}

/// Supplementary set of filters that are generic over the value type.
#[cfg(all(
    feature = "std",
    feature = "format",
    feature = "log",
    feature = "math",
    feature = "regex",
    feature = "time",
))]
pub fn generic_extra<V: ValT>() -> impl Iterator<Item = Filter<V>> {
    let filters = [std(), format(), math(), regex(), time()];
    filters.into_iter().flat_map(run_many).chain([upd(debug())])
}

/// Values that the core library can operate on.
pub trait ValT: jaq_interpret::ValT + Ord + From<f64> {
    /// Convert an array into a sequence.
    ///
    /// This returns the original value as `Err` if it is not an array.
    fn into_seq<S: FromIterator<Self>>(self) -> Result<S, Self>;

    /// Use the value as integer.
    fn as_isize(&self) -> Option<isize>;

    /// Use the value as floating-point number.
    ///
    /// This may fail in more complex ways than [`Self::as_isize`],
    /// because the value may either be
    /// not a number or a number that does not fit into [`f64`].
    fn as_f64(&self) -> Result<f64, Error<Self>>;
}

/// Convenience trait for implementing the core functions.
trait ValTx: ValT + Sized {
    fn into_vec(self) -> Result<Vec<Self>, Error<Self>> {
        self.into_seq()
            .map_err(|e| Error::Type(e, error::Type::Arr))
    }

    fn try_as_str(&self) -> Result<&str, Error<Self>> {
        self.as_str()
            .ok_or_else(|| Error::Type(self.clone(), error::Type::Str))
    }

    fn try_as_isize(&self) -> Result<isize, Error<Self>> {
        self.as_isize()
            .ok_or_else(|| Error::Type(self.clone(), error::Type::Int))
    }

    /// Use as an i32 to be given as an argument to a libm function.
    fn try_as_i32(&self) -> Result<i32, Error<Self>> {
        self.try_as_isize()?.try_into().map_err(Error::str)
    }

    /// Apply a function to an array.
    fn mutate_arr<F>(self, f: F) -> ValR2<Self>
    where
        F: FnOnce(&mut Vec<Self>) -> Result<(), Error<Self>>,
    {
        let mut a = self.into_vec()?;
        f(&mut a)?;
        Ok(Self::from_iter(a))
    }

    fn mutate_str(self, f: impl FnOnce(&mut str)) -> ValR2<Self> {
        let mut s = self.try_as_str()?.to_owned();
        f(&mut s);
        Ok(Self::from(s))
    }

    fn round(self, f: impl FnOnce(f64) -> f64) -> ValR2<Self> {
        if self.as_isize().is_some() {
            Ok(self)
        } else {
            Ok(Self::from(f(self.as_f64()?)))
        }
    }
}
impl<T: ValT> ValTx for T {}

impl ValT for Val {
    fn into_seq<S: FromIterator<Self>>(self) -> Result<S, Self> {
        match self {
            Self::Arr(a) => match Rc::try_unwrap(a) {
                Ok(a) => Ok(a.into_iter().collect()),
                Err(a) => Ok(a.iter().cloned().collect()),
            },
            _ => Err(self),
        }
    }

    fn as_isize(&self) -> Option<isize> {
        match self {
            Self::Int(i) => Some(*i),
            _ => None,
        }
    }

    fn as_f64(&self) -> Result<f64, Error> {
        Self::as_float(self)
    }
}

/// Return those named filters available by default in jaq
/// which are implemented as native filters, such as `length`, `keys`, ...,
/// but also `now`, `debug`, `fromdateiso8601`, ...
///
/// Does not return filters from the standard library, such as `map`.
#[cfg(all(
    feature = "std",
    feature = "format",
    feature = "log",
    feature = "math",
    feature = "parse_json",
    feature = "regex",
    feature = "time",
))]
pub fn core() -> impl Iterator<Item = Filter> {
    minimal().chain(generic_extra()).chain([run(PARSE_JSON)])
}

fn run_many<V>(fs: Box<[(&'static str, usize, RunPtr<V>)]>) -> impl Iterator<Item = Filter<V>> {
    fs.into_vec().into_iter().map(run)
}

fn run<V>((name, arity, run): (&str, usize, RunPtr<V>)) -> Filter<V> {
    (name.to_string(), arity, Native::new(run))
}

fn upd<V>((name, arity, run, update): (&str, usize, RunPtr<V>, UpdatePtr<V>)) -> Filter<V> {
    (name.to_string(), arity, Native::with_update(run, update))
}

/// Return 0 for null, the absolute value for numbers, and
/// the length for strings, arrays, and objects.
///
/// Fail on booleans.
fn length(v: &Val) -> ValR {
    match v {
        Val::Null => Ok(Val::Int(0)),
        Val::Bool(_) => Err(Error::str(format_args!("{v} has no length"))),
        Val::Int(i) => Ok(Val::Int(i.abs())),
        Val::Num(n) => length(&Val::from_dec_str(n)),
        Val::Float(f) => Ok(Val::Float(f.abs())),
        Val::Str(s) => Ok(Val::Int(s.chars().count() as isize)),
        Val::Arr(a) => Ok(Val::Int(a.len() as isize)),
        Val::Obj(o) => Ok(Val::Int(o.len() as isize)),
    }
}

/// Sort array by the given function.
fn sort_by<'a, V: ValT, F>(xs: &mut [V], f: F) -> Result<(), Error<V>>
where
    F: Fn(V) -> ValR2s<'a, V>,
{
    // Some(e) iff an error has previously occurred
    let mut err = None;
    xs.sort_by_cached_key(|x| run_if_ok(x.clone(), &mut err, &f));
    err.map_or(Ok(()), Err)
}

/// Group an array by the given function.
fn group_by<'a, V: ValT>(xs: Vec<V>, f: impl Fn(V) -> ValR2s<'a, V>) -> ValR2<V> {
    let mut yx: Vec<(Vec<V>, V)> = xs
        .into_iter()
        .map(|x| Ok((f(x.clone()).collect::<Result<_, _>>()?, x)))
        .collect::<Result<_, _>>()?;

    yx.sort_by(|(y1, _), (y2, _)| y1.cmp(y2));

    let mut grouped = Vec::new();
    let mut yx = yx.into_iter();
    if let Some((mut group_y, first_x)) = yx.next() {
        let mut group = Vec::from([first_x]);
        for (y, x) in yx {
            if group_y != y {
                grouped.push(V::from_iter(core::mem::take(&mut group)));
                group_y = y;
            }
            group.push(x);
        }
        if !group.is_empty() {
            grouped.push(V::from_iter(group));
        }
    }

    Ok(V::from_iter(grouped))
}

/// Get the minimum or maximum element from an array according to the given function.
fn cmp_by<'a, V: Clone, F, R>(xs: Vec<V>, f: F, replace: R) -> Result<Option<V>, Error<V>>
where
    F: Fn(V) -> ValR2s<'a, V>,
    R: Fn(&[V], &[V]) -> bool,
{
    let iter = xs.into_iter();
    let mut iter = iter.map(|x| (x.clone(), f(x).collect::<Result<Vec<_>, _>>()));
    let (mut mx, mut my) = if let Some((x, y)) = iter.next() {
        (x, y?)
    } else {
        return Ok(None);
    };
    for (x, y) in iter {
        let y = y?;
        if replace(&my, &y) {
            (mx, my) = (x, y);
        }
    }
    Ok(Some(mx))
}

/// Convert a string into an array of its Unicode codepoints.
fn explode<V: jaq_interpret::ValT>(s: &str) -> impl Iterator<Item = ValR2<V>> + '_ {
    // conversion from u32 to isize may fail on 32-bit systems for high values of c
    let conv = |c: char| Ok(isize::try_from(c as u32).map_err(Error::str)?.into());
    s.chars().map(conv)
}

/// Convert an array of Unicode codepoints into a string.
fn implode<V: ValT>(xs: &[V]) -> Result<String, Error<V>> {
    xs.iter().map(as_codepoint).collect()
}

/// If the value is an integer representing a valid Unicode codepoint, return it, else fail.
fn as_codepoint<V: ValT>(v: &V) -> Result<char, Error<V>> {
    let i = v.try_as_isize()?;
    // conversion from isize to u32 may fail on 64-bit systems for high values of c
    let u = u32::try_from(i).map_err(Error::str)?;
    // may fail e.g. on `[1114112] | implode`
    char::from_u32(u).ok_or_else(|| Error::str(format_args!("cannot use {u} as character")))
}

/// This implements a ~10x faster version of:
/// ~~~ text
/// def range($from; $to; $by): $from |
///    if $by > 0 then while(.  < $to; . + $by)
///  elif $by < 0 then while(.  > $to; . + $by)
///    else            while(. != $to; . + $by)
///    end;
/// ~~~
fn range<V: ValT>(mut from: ValR2<V>, to: V, by: V) -> impl Iterator<Item = ValR2<V>> {
    use core::cmp::Ordering::{Equal, Greater, Less};
    let cmp = by.partial_cmp(&V::from(0)).unwrap_or(Equal);
    core::iter::from_fn(move || match from.clone() {
        Ok(x) => match cmp {
            Greater => x < to,
            Less => x > to,
            Equal => x != to,
        }
        .then(|| core::mem::replace(&mut from, x + by.clone())),
        e @ Err(_) => {
            // return None after the error
            from = Ok(to.clone());
            Some(e)
        }
    })
}

/// Return the string windows having `n` characters, where `n` > 0.
///
/// Taken from <https://users.rust-lang.org/t/iterator-over-windows-of-chars/17841/3>.
fn str_windows(line: &str, n: usize) -> impl Iterator<Item = &str> {
    line.char_indices()
        .zip(line.char_indices().skip(n).chain(Some((line.len(), ' '))))
        .map(move |((i, _), (j, _))| &line[i..j])
}

/// Return the indices of `y` in `x`.
fn indices<'a>(x: &'a Val, y: &'a Val) -> Result<Box<dyn Iterator<Item = usize> + 'a>, Error> {
    match (x, y) {
        (Val::Str(_), Val::Str(y)) if y.is_empty() => Ok(Box::new(core::iter::empty())),
        (Val::Arr(_), Val::Arr(y)) if y.is_empty() => Ok(Box::new(core::iter::empty())),
        (Val::Str(x), Val::Str(y)) => {
            let iw = str_windows(x, y.chars().count()).enumerate();
            Ok(Box::new(iw.filter_map(|(i, w)| (w == **y).then_some(i))))
        }
        (Val::Arr(x), Val::Arr(y)) => {
            let iw = x.windows(y.len()).enumerate();
            Ok(Box::new(iw.filter_map(|(i, w)| (w == **y).then_some(i))))
        }
        (Val::Arr(x), y) => {
            let ix = x.iter().enumerate();
            Ok(Box::new(ix.filter_map(move |(i, x)| (x == y).then_some(i))))
        }
        (x, y) => Err(Error::Index(x.clone(), y.clone())),
    }
}

fn once_with<'a, T>(f: impl FnOnce() -> T + 'a) -> Box<dyn Iterator<Item = T> + 'a> {
    Box::new(core::iter::once_with(f))
}

fn once_or_empty<'a, T>(f: impl FnOnce() -> Option<T> + 'a) -> Box<dyn Iterator<Item = T> + 'a> {
    Box::new(core::iter::once_with(f).flatten())
}

fn unary<'a, V: jaq_interpret::ValT, F>(args: Args<'a, V>, cv: Cv<'a, V>, f: F) -> ValR2s<'a, V>
where
    F: Fn(&V, V) -> ValR2<V> + 'a,
{
    let vals = args.get(0).run(cv.clone());
    Box::new(vals.map(move |y| f(&cv.1, y?)))
}

macro_rules! ow {
    ( $f:expr ) => {
        once_with(move || $f)
    };
}

const JSON_MINIMAL: &[(&str, usize, RunPtr)] = &[
    ("length", 0, |_, cv| ow!(length(&cv.1))),
    ("keys_unsorted", 0, |_, cv| {
        ow!(cv.1.keys_unsorted().map(Val::arr))
    }),
    ("contains", 1, |args, cv| {
        unary(args, cv, |x, y| Ok(Val::from(x.contains(&y))))
    }),
    ("has", 1, |args, cv| {
        unary(args, cv, |v, k| v.has(&k).map(Val::from))
    }),
    ("indices", 1, |args, cv| {
        let to_int = |i: usize| Val::Int(i.try_into().unwrap());
        unary(args, cv, move |x, v| {
            indices(x, &v).map(|idxs| idxs.map(to_int).collect())
        })
    }),
];

#[allow(clippy::unit_arg)]
fn core_run<V: ValT>() -> Box<[(&'static str, usize, RunPtr<V>)]> {
    Box::new([
        ("inputs", 0, |_, cv| {
            Box::new(cv.0.inputs().map(|r| r.map_err(Error::str)))
        }),
        ("tojson", 0, |_, cv| ow!(Ok(cv.1.to_string().into()))),
        ("floor", 0, |_, cv| ow!(cv.1.round(f64::floor))),
        ("round", 0, |_, cv| ow!(cv.1.round(f64::round))),
        ("ceil", 0, |_, cv| ow!(cv.1.round(f64::ceil))),
        ("utf8bytelength", 0, |_, cv| {
            ow!(cv.1.try_as_str().map(|s| (s.len() as isize).into()))
        }),
        ("explode", 0, |_, cv| {
            ow!(cv.1.try_as_str().and_then(|s| explode(s).collect()))
        }),
        ("implode", 0, |_, cv| {
            ow!(cv.1.into_vec().and_then(|s| implode(&s)).map(V::from))
        }),
        ("ascii_downcase", 0, |_, cv| {
            ow!(cv.1.mutate_str(str::make_ascii_lowercase))
        }),
        ("ascii_upcase", 0, |_, cv| {
            ow!(cv.1.mutate_str(str::make_ascii_uppercase))
        }),
        ("reverse", 0, |_, cv| {
            ow!(cv.1.mutate_arr(|a| Ok(a.reverse())))
        }),
        ("sort", 0, |_, cv| ow!(cv.1.mutate_arr(|a| Ok(a.sort())))),
        ("sort_by", 1, |args, cv| {
            let f = move |v| args.get(0).run((cv.0.clone(), v));
            ow!(cv.1.mutate_arr(|a| sort_by(a, f)))
        }),
        ("group_by", 1, |args, cv| {
            let f = move |v| args.get(0).run((cv.0.clone(), v));
            ow!(cv.1.into_vec().and_then(|a| group_by(a, f)))
        }),
        ("min_by_or_empty", 1, |args, cv| {
            let f = move |a| cmp_by(a, |v| args.get(0).run((cv.0.clone(), v)), |my, y| y < my);
            once_or_empty(move || cv.1.into_vec().and_then(f).transpose())
        }),
        ("max_by_or_empty", 1, |args, cv| {
            let f = move |a| cmp_by(a, |v| args.get(0).run((cv.0.clone(), v)), |my, y| y >= my);
            once_or_empty(move || cv.1.into_vec().and_then(f).transpose())
        }),
        ("first", 1, |args, cv| Box::new(args.get(0).run(cv).take(1))),
        ("limit", 2, |args, cv| {
            let n = args.get(0).run(cv.clone()).map(|n| n?.try_as_isize());
            let f = move |n| args.get(1).run(cv.clone()).take(n);
            let pos = |n: isize| n.try_into().unwrap_or(0usize);
            Box::new(n.flat_map(move |n| then(n, |n| Box::new(f(pos(n))))))
        }),
        ("range", 3, |args, cv| {
            let (from, to, by) = (args.get(0), args.get(1), args.get(2));
            Box::new(from.cartesian3(to, by, cv).flat_map(|(from, to, by)| {
                let to_by = to.and_then(|to| Ok((to, by?)));
                then(to_by, |(to, by)| Box::new(range(from, to, by)))
            }))
        }),
        ("startswith", 1, |args, cv| {
            unary(args, cv, |v, s| {
                Ok(v.try_as_str()?.starts_with(s.try_as_str()?).into())
            })
        }),
        ("endswith", 1, |args, cv| {
            unary(args, cv, |v, s| {
                Ok(v.try_as_str()?.ends_with(s.try_as_str()?).into())
            })
        }),
        ("ltrimstr", 1, |args, cv| {
            unary(args, cv, |v, pre| {
                Ok(v.try_as_str()?
                    .strip_prefix(pre.try_as_str()?)
                    .map_or_else(|| v.clone(), |s| V::from(s.to_owned())))
            })
        }),
        ("rtrimstr", 1, |args, cv| {
            unary(args, cv, |v, suf| {
                Ok(v.try_as_str()?
                    .strip_suffix(suf.try_as_str()?)
                    .map_or_else(|| v.clone(), |s| V::from(s.to_owned())))
            })
        }),
        ("escape_csv", 0, |_, cv| {
            ow!(Ok(cv.1.try_as_str()?.replace('"', "\"\"").into()))
        }),
        ("escape_sh", 0, |_, cv| {
            ow!(Ok(cv.1.try_as_str()?.replace('\'', r"'\''").into()))
        }),
    ])
}

#[cfg(feature = "std")]
fn now<V: From<String>>() -> Result<f64, Error<V>> {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|x| x.as_secs_f64())
        .map_err(Error::str)
}

#[cfg(feature = "std")]
fn std<V: ValT>() -> Box<[(&'static str, usize, RunPtr<V>)]> {
    use std::env::vars;
    Box::new([
        ("env", 0, |_, _| {
            ow!(V::from_map(vars().map(|(k, v)| (V::from(k), V::from(v)))))
        }),
        ("now", 0, |_, _| ow!(now().map(V::from))),
    ])
}

#[cfg(feature = "parse_json")]
/// Convert string to a single JSON value.
fn from_json(s: &str) -> ValR {
    use hifijson::token::Lex;
    let mut lexer = hifijson::SliceLexer::new(s.as_bytes());
    lexer
        .exactly_one(Val::parse)
        .map_err(|e| Error::str(format_args!("cannot parse {s} as JSON: {e}")))
}

#[cfg(feature = "parse_json")]
const PARSE_JSON: (&str, usize, RunPtr) = ("fromjson", 0, |_, cv| {
    ow!(cv.1.as_str().and_then(|s| from_json(s)))
});

#[cfg(feature = "format")]
fn replace(s: &str, patterns: &[&str], replacements: &[&str]) -> String {
    let ac = aho_corasick::AhoCorasick::new(patterns).unwrap();
    ac.replace_all(s, replacements)
}

#[cfg(feature = "format")]
fn format<V: ValT>() -> Box<[(&'static str, usize, RunPtr<V>)]> {
    Box::new([
        ("escape_html", 0, |_, cv| {
            let pats = ["<", ">", "&", "\'", "\""];
            let reps = ["&lt;", "&gt;", "&amp;", "&apos;", "&quot;"];
            ow!(Ok(replace(cv.1.try_as_str()?, &pats, &reps).into()))
        }),
        ("escape_tsv", 0, |_, cv| {
            let pats = ["\n", "\r", "\t", "\\"];
            let reps = ["\\n", "\\r", "\\t", "\\\\"];
            ow!(Ok(replace(cv.1.try_as_str()?, &pats, &reps).into()))
        }),
        ("encode_uri", 0, |_, cv| {
            use urlencoding::encode;
            ow!(Ok(encode(cv.1.try_as_str()?).into_owned().into()))
        }),
        ("encode_base64", 0, |_, cv| {
            use base64::{engine::general_purpose::STANDARD, Engine};
            ow!(Ok(STANDARD.encode(cv.1.try_as_str()?).into()))
        }),
        ("decode_base64", 0, |_, cv| {
            use base64::{engine::general_purpose::STANDARD, Engine};
            use core::str::from_utf8;
            ow!({
                let d = STANDARD.decode(cv.1.try_as_str()?).map_err(Error::str)?;
                Ok(from_utf8(&d).map_err(Error::str)?.to_owned().into())
            })
        }),
    ])
}

#[cfg(feature = "math")]
fn math<V: ValT>() -> Box<[(&'static str, usize, RunPtr<V>)]> {
    let rename = |name, (_name, arity, f): (&'static str, usize, RunPtr<V>)| (name, arity, f);
    Box::new([
        math::f_f!(acos),
        math::f_f!(acosh),
        math::f_f!(asin),
        math::f_f!(asinh),
        math::f_f!(atan),
        math::f_f!(atanh),
        math::f_f!(cbrt),
        math::f_f!(cos),
        math::f_f!(cosh),
        math::f_f!(erf),
        math::f_f!(erfc),
        math::f_f!(exp),
        math::f_f!(exp10),
        math::f_f!(exp2),
        math::f_f!(expm1),
        math::f_f!(fabs),
        math::f_fi!(frexp),
        math::f_i!(ilogb),
        math::f_f!(j0),
        math::f_f!(j1),
        math::f_f!(lgamma),
        math::f_f!(log),
        math::f_f!(log10),
        math::f_f!(log1p),
        math::f_f!(log2),
        // logb is implemented in jaq-std
        math::f_ff!(modf),
        rename("nearbyint", math::f_f!(round)),
        // pow10 is implemented in jaq-std
        math::f_f!(rint),
        // significand is implemented in jaq-std
        math::f_f!(sin),
        math::f_f!(sinh),
        math::f_f!(sqrt),
        math::f_f!(tan),
        math::f_f!(tanh),
        math::f_f!(tgamma),
        math::f_f!(trunc),
        math::f_f!(y0),
        math::f_f!(y1),
        math::ff_f!(atan2),
        math::ff_f!(copysign),
        // drem is implemented in jaq-std
        math::ff_f!(fdim),
        math::ff_f!(fmax),
        math::ff_f!(fmin),
        math::ff_f!(fmod),
        math::ff_f!(hypot),
        math::if_f!(jn),
        math::fi_f!(ldexp),
        math::ff_f!(nextafter),
        // nexttoward is implemented in jaq-std
        math::ff_f!(pow),
        math::ff_f!(remainder),
        // scalb is implemented in jaq-std
        rename("scalbln", math::fi_f!(scalbn)),
        math::if_f!(yn),
        math::fff_f!(fma),
    ])
}

type Cv<'a, V = Val> = (jaq_interpret::Ctx<'a, V>, V);

#[cfg(feature = "regex")]
fn re<'a, V: ValT, F>(re: F, flags: F, s: bool, m: bool, cv: Cv<'a, V>) -> ValR2s<'a, V>
where
    F: FilterT<'a, V>,
{
    let re_flags = re.cartesian(flags, (cv.0, cv.1.clone()));

    Box::new(re_flags.map(move |(re, flags)| {
        use crate::regex::Part::{Matches, Mismatch};
        let fail_flag = |e| Error::str(format_args!("invalid regex flag: {e}"));
        let fail_re = |e| Error::str(format_args!("invalid regex: {e}"));

        let flags = regex::Flags::new(flags?.try_as_str()?).map_err(fail_flag)?;
        let re = flags.regex(re?.try_as_str()?).map_err(fail_re)?;
        let out = regex::regex(cv.1.try_as_str()?, &re, flags, (s, m));
        let out = out.into_iter().map(|out| match out {
            Matches(ms) => ms.into_iter().map(|m| V::from_map(m.fields())).collect(),
            Mismatch(s) => Ok(V::from(s.to_string())),
        });
        out.collect()
    }))
}

#[cfg(feature = "regex")]
fn regex<V: ValT>() -> Box<[(&'static str, usize, RunPtr<V>)]> {
    Box::new([
        ("matches", 2, |args, cv| {
            re(args.get(0), args.get(1), false, true, cv)
        }),
        ("split_matches", 2, |args, cv| {
            re(args.get(0), args.get(1), true, true, cv)
        }),
        ("split_", 2, |args, cv| {
            re(args.get(0), args.get(1), true, false, cv)
        }),
    ])
}

#[cfg(feature = "time")]
fn time<V: ValT>() -> Box<[(&'static str, usize, RunPtr<V>)]> {
    Box::new([
        ("fromdateiso8601", 0, |_, cv| {
            ow!(time::from_iso8601(cv.1.try_as_str()?))
        }),
        ("todateiso8601", 0, |_, cv| {
            ow!(Ok(time::to_iso8601(&cv.1)?.into()))
        }),
    ])
}

fn error<V>() -> (&'static str, usize, RunPtr<V>, UpdatePtr<V>) {
    (
        "error",
        0,
        |_, cv| ow!(Err(Error::Val(cv.1))),
        |_, cv, _| ow!(Err(Error::Val(cv.1))),
    )
}

#[cfg(feature = "log")]
fn with_debug<T: core::fmt::Display>(x: T) -> T {
    log::debug!("{}", x);
    x
}

#[cfg(feature = "log")]
fn debug<V: core::fmt::Display>() -> (&'static str, usize, RunPtr<V>, UpdatePtr<V>) {
    (
        "debug",
        0,
        |_, cv| ow!(Ok(with_debug(cv.1))),
        |_, cv, f| f(with_debug(cv.1)),
    )
}

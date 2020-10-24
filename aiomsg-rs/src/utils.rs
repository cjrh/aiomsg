use std::time::Duration;

pub fn hexify(data: &[u8], maxlen: usize) -> String {
    data.iter()
        .take(maxlen)
        .map(|byte| format!("{:02x?}", byte))
        .collect::<Vec<String>>()
        .join("")
}

pub fn _hexify_vec(data: &[&[u8]], maxlen: usize) -> Vec<String> {
    data.iter().map(|v| hexify(v, maxlen)).collect()
}

pub fn stringify(data: &[u8], maxlen: usize) -> String {
    String::from_utf8_lossy(data).chars().take(maxlen).collect()
}

pub async fn sleep<T: Into<f64>>(secs: T) {
    async_std::task::sleep(Duration::from_secs_f64(secs.into())).await;
}

/// Return an iterator that produces an exponential backoff sequence. You can specify what the
/// start value is, the desired end value, and the number of steps it should take to get to the
/// end value.
///
/// ```
/// use aiomsg_rs::utils::backoff_seq;
/// let v = backoff_seq(0.1, 5, 3).take(5).collect::<Vec<f64>>();
/// let expected: Vec<f64> = vec![0.1, 0.7071067811865476, 5.000000000000001, 5.0, 5.0].iter().map(|x| *x as f64).collect();
/// assert_eq!(v, expected);
/// ```
pub fn backoff_seq<T: Into<f64>, U: Into<f64>>(
    start: T,
    end: U,
    n: i32,
) -> impl Iterator<Item = f64> {
    let a = start.into();
    let xn = end.into();
    let exp = 1f64 / (f64::from(n) - 1f64);
    let r = (xn / a).powf(exp);
    let rng = (1..(n + 1));
    rng.map(move |nn| a * r.powi(nn - 1))
        .chain(std::iter::repeat(xn))
}

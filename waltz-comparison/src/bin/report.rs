use anyhow::{Context, bail};
use serde::Serialize;
use serde_json::Value;
use std::{
    collections::BTreeMap,
    env, fs,
    path::{Path, PathBuf},
    process::Command,
    thread,
};

const PACKAGES: [&str; 3] = ["kameo", "ractor", "waltz"];

fn main() -> anyhow::Result<()> {
    let args = Args::from_env()?;

    let measurements = read_measurements(&args.input)?;
    if measurements.is_empty() {
        bail!("no criterion results below {}", args.input.display());
    }

    let report = Report {
        metadata: read_metadata(args.tag)?,
        measurements,
    };

    fs::create_dir_all(&args.output)
        .with_context(|| format!("output directory {}", args.output.display()))?;

    let json = serde_json::to_string_pretty(&report).context("report as JSON")?;
    fs::write(args.output.join("results.json"), json).context("results.json")?;
    fs::write(args.output.join("index.html"), render_html(&report)).context("index.html")?;

    println!(
        "wrote {} measurements for {} to {}",
        report.measurements.len(),
        report.metadata.tag,
        args.output.display()
    );

    Ok(())
}

struct Args {
    input: PathBuf,
    output: PathBuf,
    tag: String,
}

impl Args {
    fn from_env() -> anyhow::Result<Self> {
        let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");

        let mut input = workspace.join("target/criterion-comparison");
        let mut output = workspace.join("target/comparison-report");
        let mut tag = env::var("GITHUB_REF_NAME").unwrap_or_else(|_| "local".to_string());

        let mut args = env::args().skip(1);
        while let Some(arg) = args.next() {
            let mut value = || args.next().with_context(|| format!("value for {arg}"));

            match arg.as_str() {
                "--input" => input = PathBuf::from(value()?),
                "--output" => output = PathBuf::from(value()?),
                "--tag" => tag = value()?,
                _ => bail!("unexpected argument {arg}"),
            }
        }

        Ok(Args { input, output, tag })
    }
}

#[derive(Serialize)]
struct Report {
    metadata: Metadata,
    measurements: Vec<Measurement>,
}

#[derive(Serialize)]
struct Metadata {
    tag: String,
    commit: Option<String>,
    date: Option<String>,
    os: Option<String>,
    cpu: Option<String>,
    cores: Option<usize>,
    rustc: Option<String>,
    versions: BTreeMap<String, String>,
}

#[derive(Serialize)]
struct Measurement {
    group: String,
    framework: String,
    parameter: Option<String>,
    elements: u64,
    mean_ns: f64,
    lower_ns: f64,
    upper_ns: f64,
    elements_per_second: f64,
}

impl Measurement {
    fn key(&self) -> (&str, Option<&str>) {
        (&self.group, self.parameter.as_deref())
    }
}

fn read_measurements(input: &Path) -> anyhow::Result<Vec<Measurement>> {
    let directories = collect_sample_directories(input)
        .with_context(|| format!("walking {}", input.display()))?;

    let mut measurements = directories
        .iter()
        .map(|directory| read_measurement(directory))
        .collect::<anyhow::Result<Vec<_>>>()?;

    measurements.sort_by(|a, b| {
        a.group
            .cmp(&b.group)
            .then_with(|| parameter_order(a).cmp(&parameter_order(b)))
            .then_with(|| a.framework.cmp(&b.framework))
    });

    Ok(measurements)
}

fn collect_sample_directories(directory: &Path) -> anyhow::Result<Vec<PathBuf>> {
    if !directory.is_dir() {
        return Ok(Vec::new());
    }

    let collected = fs::read_dir(directory)
        .with_context(|| format!("reading directory {}", directory.display()))?
        .map(|entry| {
            let path = entry
                .with_context(|| format!("entry below {}", directory.display()))?
                .path();
            if !path.is_dir() {
                Ok(Vec::new())
            } else if path.file_name().and_then(|name| name.to_str()) == Some("new")
                && path.join("benchmark.json").is_file()
                && path.join("estimates.json").is_file()
            {
                Ok(vec![path])
            } else {
                collect_sample_directories(&path)
            }
        })
        .collect::<anyhow::Result<Vec<_>>>()?;

    Ok(collected.into_iter().flatten().collect())
}

fn read_measurement(directory: &Path) -> anyhow::Result<Measurement> {
    let benchmark = read_json(&directory.join("benchmark.json"))?;
    let estimates = read_json(&directory.join("estimates.json"))?;

    let group = string(&benchmark, "/group_id")?;
    let framework = string(&benchmark, "/function_id")?;
    let parameter = benchmark
        .pointer("/value_str")
        .and_then(Value::as_str)
        .map(ToString::to_string);

    let elements = count(&benchmark, "/throughput/Elements")
        .with_context(|| format!("element throughput of {group}/{framework}"))?;

    let mean_ns = number(&estimates, "/mean/point_estimate")?;
    let lower_ns = number(&estimates, "/mean/confidence_interval/lower_bound")?;
    let upper_ns = number(&estimates, "/mean/confidence_interval/upper_bound")?;

    Ok(Measurement {
        group,
        framework,
        parameter,
        elements,
        mean_ns,
        lower_ns,
        upper_ns,
        elements_per_second: elements as f64 * 1e9 / mean_ns,
    })
}

fn read_json(path: &Path) -> anyhow::Result<Value> {
    let contents =
        fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    serde_json::from_str(&contents).with_context(|| format!("parsing {}", path.display()))
}

fn string(value: &Value, pointer: &str) -> anyhow::Result<String> {
    value
        .pointer(pointer)
        .and_then(Value::as_str)
        .map(ToString::to_string)
        .with_context(|| format!("string field {pointer}"))
}

fn count(value: &Value, pointer: &str) -> anyhow::Result<u64> {
    value
        .pointer(pointer)
        .and_then(Value::as_u64)
        .with_context(|| format!("integer field {pointer}"))
}

fn number(value: &Value, pointer: &str) -> anyhow::Result<f64> {
    value
        .pointer(pointer)
        .and_then(Value::as_f64)
        .with_context(|| format!("number field {pointer}"))
}

fn parameter_order(measurement: &Measurement) -> ParameterOrder<'_> {
    match &measurement.parameter {
        Some(parameter) => match parameter.parse() {
            Ok(number) => ParameterOrder::Number(number),
            Err(_) => ParameterOrder::Text(parameter),
        },
        None => ParameterOrder::Missing,
    }
}

// Derived `Ord` compares by variant first, hence the declaration order is the sort order.
#[derive(PartialEq, Eq, PartialOrd, Ord)]
enum ParameterOrder<'a> {
    Missing,
    Number(u64),
    Text(&'a str),
}

fn read_metadata(tag: String) -> anyhow::Result<Metadata> {
    Ok(Metadata {
        tag,
        commit: or_none(env::var("GITHUB_SHA").or_else(|_| run("git", &["rev-parse", "HEAD"]))),
        date: or_none(run("date", &["-u", "+%Y-%m-%dT%H:%M:%SZ"])),
        os: or_none(run("uname", &["-sr"])),
        cpu: read_cpu(),
        cores: thread::available_parallelism()
            .map(|cores| cores.get())
            .ok(),
        rustc: or_none(run("rustc", &["--version"])),
        versions: read_versions()?,
    })
}

fn read_cpu() -> Option<String> {
    let from_cpuinfo = fs::read_to_string("/proc/cpuinfo")
        .ok()
        .and_then(|cpuinfo| model_name(&cpuinfo));

    from_cpuinfo.or_else(|| {
        or_none(
            run("lscpu", &[])
                .and_then(|lscpu| model_name(&lscpu).context("model name in lscpu output")),
        )
    })
}

fn model_name(text: &str) -> Option<String> {
    text.lines()
        .find(|line| line.to_lowercase().starts_with("model name"))
        .and_then(|line| line.split_once(':'))
        .map(|(_, model)| model.trim().to_string())
        .filter(|model| !model.is_empty() && model != "-")
}

fn read_versions() -> anyhow::Result<BTreeMap<String, String>> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../Cargo.lock");
    let lock = fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;

    let mut versions = BTreeMap::new();
    let mut name = None;
    for line in lock.lines() {
        if let Some(value) = line.strip_prefix("name = ") {
            name = Some(value.trim_matches('"').to_string());
        } else if let Some(value) = line.strip_prefix("version = ")
            && let Some(name) = name.take()
            && PACKAGES.contains(&name.as_str())
        {
            versions.insert(name, value.trim_matches('"').to_string());
        }
    }

    let missing = PACKAGES
        .iter()
        .filter(|package| !versions.contains_key(**package))
        .copied()
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        bail!(
            "no version in {} for {}",
            path.display(),
            missing.join(", ")
        );
    }

    Ok(versions)
}

// Keep the caveats below in sync with the ones in the README.
fn render_html(report: &Report) -> String {
    let sections = report
        .measurements
        .chunk_by(|a, b| a.key() == b.key())
        .map(|measurements| {
            let (group, parameter) = measurements[0].key();
            render_section(group, parameter, measurements)
        })
        .collect::<String>();

    let metadata = &report.metadata;
    let versions = metadata
        .versions
        .iter()
        .map(|(name, version)| format!("{name} {version}"))
        .collect::<Vec<_>>()
        .join(", ");

    format!(
        r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>waltz comparison benchmarks ({tag})</title>
<style>
:root {{ color-scheme: light dark; --fg: #111; --muted: #555; --bg: #fff; --line: #ddd; --bar: #4f7cff; --warn-bg: #fff8e1; --warn-line: #e6c200; }}
@media (prefers-color-scheme: dark) {{
  :root {{ --fg: #e6e6e6; --muted: #a0a0a0; --bg: #14161a; --line: #333; --bar: #6d92ff; --warn-bg: #2a2413; --warn-line: #7a6a1a; }}
}}
* {{ box-sizing: border-box; }}
body {{ margin: 0 auto; padding: 2rem 1rem; max-width: 60rem; background: var(--bg); color: var(--fg);
  font: 16px/1.6 system-ui, -apple-system, Segoe UI, Roboto, sans-serif; }}
h1 {{ font-size: 1.6rem; margin: 0 0 .25rem; }}
h2 {{ font-size: 1.15rem; margin: 2.5rem 0 .75rem; }}
a {{ color: inherit; }}
.sub {{ color: var(--muted); margin: 0 0 1.5rem; }}
.meta {{ display: grid; grid-template-columns: max-content 1fr; gap: .25rem 1rem; padding: 1rem;
  border: 1px solid var(--line); border-radius: 8px; font-size: .9rem; }}
.meta dt {{ color: var(--muted); }}
.meta dd {{ margin: 0; overflow-wrap: anywhere; }}
.caveats {{ margin: 1.5rem 0; padding: 1rem 1rem 1rem 1.25rem; background: var(--warn-bg);
  border-left: 4px solid var(--warn-line); border-radius: 4px; font-size: .92rem; }}
.caveats h2 {{ margin: 0 0 .5rem; font-size: 1rem; }}
.caveats ol {{ margin: 0; padding-left: 1.1rem; }}
.caveats li {{ margin: .4rem 0; }}
.scroll {{ overflow-x: auto; }}
table {{ border-collapse: collapse; width: 100%; font-size: .92rem; }}
th, td {{ text-align: left; padding: .45rem .6rem; border-bottom: 1px solid var(--line); white-space: nowrap; }}
th {{ color: var(--muted); font-weight: 600; }}
td.num {{ text-align: right; font-variant-numeric: tabular-nums; }}
.bar {{ display: block; height: .55rem; background: var(--bar); border-radius: 3px; min-width: 2px; }}
td.barcell {{ width: 40%; }}
footer {{ margin-top: 3rem; color: var(--muted); font-size: .85rem; }}
</style>
</head>
<body>
<h1>waltz comparison benchmarks</h1>
<p class="sub">Messaging throughput of <strong>waltz</strong> against kameo and ractor, higher is better.</p>

<dl class="meta">
<dt>Tag</dt><dd>{tag}</dd>
<dt>Commit</dt><dd>{commit}</dd>
<dt>Date</dt><dd>{date}</dd>
<dt>Versions</dt><dd>{versions}</dd>
<dt>CPU</dt><dd>{cpu} ({cores} cores)</dd>
<dt>OS</dt><dd>{os}</dd>
<dt>Toolchain</dt><dd>{rustc}</dd>
</dl>

<div class="caveats">
<h2>Read this before drawing conclusions</h2>
<ol>
<li><strong>waltz's <code>receive</code> is synchronous; kameo's and ractor's handlers are <code>async fn</code>.</strong>
waltz therefore avoids allocating and polling a future per message, but cannot await inside <code>receive</code>.
This is a capability difference, not only a speed difference, and it favours waltz on exactly these microbenchmarks.</li>
<li><strong>waltz's mailbox is statically typed; the others erase message types</strong>, costing an allocation
and a dynamic dispatch per message that waltz does not pay.</li>
<li><strong>Competitors are configured for speed, not defaults.</strong> kameo runs without <code>tracing</code>
and ractor without <code>message_span_propogation</code>, both of which waltz has no equivalent of. This biases
the setup in the competitors' favour.</li>
<li><strong>Messaging microbenchmarks only.</strong> Nothing here speaks to supervision, distribution,
ergonomics, memory use or production readiness. kameo and ractor are mature, feature-rich frameworks; waltz is
under active development and does far less.</li>
<li><strong>On CI these run on a shared 2-core runner</strong>, so absolute figures are not representative of real
deployments. Only the relative comparison within a single run is meaningful.</li>
<li><strong>Written and run by waltz's maintainer.</strong> The full methodology and benchmark source are in the
repository; corrections are welcome.</li>
</ol>
</div>
{sections}
<footer>
Generated from criterion results. See the
<a href="../../dev/bench/">waltz regression benchmarks</a> for throughput over time.
</footer>
</body>
</html>
"#,
        tag = escape(&metadata.tag),
        commit = escape(or_unknown(metadata.commit.as_deref())),
        date = escape(or_unknown(metadata.date.as_deref())),
        versions = escape(&versions),
        cpu = escape(or_unknown(metadata.cpu.as_deref())),
        cores = metadata
            .cores
            .map_or_else(|| "unknown".to_string(), |cores| cores.to_string()),
        os = escape(or_unknown(metadata.os.as_deref())),
        rustc = escape(or_unknown(metadata.rustc.as_deref())),
    )
}

fn render_section(group: &str, parameter: Option<&str>, measurements: &[Measurement]) -> String {
    let fastest = measurements
        .iter()
        .map(|measurement| measurement.elements_per_second)
        .fold(0.0, f64::max);

    let heading = match parameter {
        Some(parameter) => format!("{group} ({parameter})"),
        None => group.to_string(),
    };

    let rows = measurements
        .iter()
        .map(|measurement| {
            let relative = if fastest > 0.0 {
                measurement.elements_per_second / fastest
            } else {
                0.0
            };
            let width = relative * 100.0;

            format!(
                r#"<tr><td>{framework}</td><td class="num">{mean:.3} ms</td><td class="num">{throughput:.2} M/s</td><td class="num">{relative:.2}x</td><td class="barcell"><span class="bar" style="width:{width:.1}%"></span></td></tr>"#,
                framework = escape(&measurement.framework),
                mean = measurement.mean_ns / 1e6,
                throughput = measurement.elements_per_second / 1e6,
            )
        })
        .collect::<String>();

    format!(
        r#"<h2>{heading}</h2>
<div class="scroll"><table>
<thead><tr><th>Framework</th><th class="num">Time</th><th class="num">Throughput</th><th class="num">vs fastest</th><th></th></tr></thead>
<tbody>{rows}</tbody>
</table></div>
"#,
        heading = escape(&heading),
    )
}

fn run(program: &str, args: &[&str]) -> anyhow::Result<String> {
    let output = Command::new(program)
        .args(args)
        .output()
        .with_context(|| format!("running {program}"))?;
    if !output.status.success() {
        bail!("{program} exited with {}", output.status);
    }

    let stdout =
        String::from_utf8(output.stdout).with_context(|| format!("{program} stdout as UTF-8"))?;

    Ok(stdout.trim().to_string())
}

fn or_none(value: anyhow::Result<String>) -> Option<String> {
    value
        .inspect_err(|error| eprintln!("metadata unavailable: {error:#}"))
        .ok()
}

fn or_unknown(value: Option<&str>) -> &str {
    value.unwrap_or("unknown")
}

fn escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

use ceri_engine::{
    APP_ORIGIN, Browser, EngineHost, FontFace, FontSet, InputEvent, ResourceBundle, Size,
};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

struct Options {
    input: PathBuf,
    output: PathBuf,
    viewport: Size,
    scale: f32,
    clicks: Vec<String>,
    click_points: Vec<(f32, f32)>,
    pointer_moves: Vec<(f32, f32)>,
    text_inputs: Vec<String>,
    keys: Vec<String>,
    url: String,
    asset_root: Option<PathBuf>,
    ticks: Vec<u64>,
    font: Option<PathBuf>,
    bold_font: Option<PathBuf>,
    frames: usize,
    profile: bool,
}

fn main() -> Result<(), String> {
    let options = parse_args(env::args_os().skip(1))?;
    let source = fs::read_to_string(&options.input)
        .map_err(|error| format!("could not read {}: {error}", options.input.display()))?;
    let asset_root = options
        .asset_root
        .as_deref()
        .map(Path::to_path_buf)
        .or_else(|| {
            options
                .input
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
                .map(Path::to_path_buf)
        })
        .unwrap_or_else(|| PathBuf::from("."));
    let mut resources = ResourceBundle::from_directory(&asset_root).map_err(|error| {
        format!(
            "could not read asset root {}: {error}",
            asset_root.display()
        )
    })?;
    resources
        .insert(
            "/index.html",
            "text/html; charset=utf-8",
            source.into_bytes(),
        )
        .map_err(|error| error.to_string())?;
    let mut browser = Browser::new(resources);
    if let Some(path) = options.font.as_deref() {
        let regular = load_font(path)?;
        let bold = match options.bold_font.as_deref() {
            Some(path) => Some(load_font(path)?),
            None => None,
        };
        browser = browser.with_fonts(std::rc::Rc::new(FontSet::new(regular, bold)));
    }
    let mut host = EngineHost::with_scale(browser, options.viewport, options.scale);
    let status = host
        .navigate(&options.url)
        .map_err(|error| error.to_string())?;
    println!("navigation {}: {status}", options.url);
    for &(x, y) in &options.pointer_moves {
        let result = host
            .input(InputEvent::PointerMove { x, y })
            .map_err(|error| error.to_string())?;
        println!(
            "pointer move @{x},{y}: {}",
            if result.handled {
                "state changed"
            } else {
                "unchanged"
            }
        );
    }
    for id in &options.clicks {
        let dispatched = host.click_id(id).map_err(|error| error.to_string())?;
        println!(
            "click #{id}: {}",
            if dispatched { "handled" } else { "no listener" }
        );
    }
    for &(x, y) in &options.click_points {
        let result = host
            .input(InputEvent::Click { x, y })
            .map_err(|error| error.to_string())?;
        println!(
            "click @{x},{y}: {}",
            if result.handled {
                "handled"
            } else {
                "no listener"
            }
        );
    }
    for text in &options.text_inputs {
        let result = host
            .input(InputEvent::TextInput { text: text.clone() })
            .map_err(|error| error.to_string())?;
        println!(
            "text input {:?}: {}",
            text,
            if result.handled { "handled" } else { "ignored" }
        );
    }
    for key in &options.keys {
        let result = host
            .input(InputEvent::KeyDown { key: key.clone() })
            .map_err(|error| error.to_string())?;
        println!(
            "key {:?}: {}",
            key,
            if result.handled { "handled" } else { "ignored" }
        );
    }
    for milliseconds in &options.ticks {
        let executed = host
            .input(InputEvent::AdvanceTime(*milliseconds))
            .map_err(|error| error.to_string())?;
        println!("advanced {milliseconds}ms: {} task(s)", executed.tasks_run);
    }
    let timed = if options.frames > 0 {
        report_frame_times(&mut host, options.frames, options.profile)?
    } else {
        None
    };
    let layout = host
        .browser()
        .layout(options.viewport)
        .map_err(|error| error.to_string())?;
    // after a timed run the page may have nothing new to paint, so its last frame stands
    let frame = host
        .render()
        .map_err(|error| error.to_string())?
        .or(timed)
        .ok_or_else(|| "host did not produce a frame".to_string())?;
    let canvas = frame.canvas;
    if let Some(parent) = options
        .output
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)
            .map_err(|error| format!("could not create {}: {error}", parent.display()))?;
    }
    let write_image = match options
        .output
        .extension()
        .and_then(|extension| extension.to_str())
    {
        Some(extension) if extension.eq_ignore_ascii_case("bmp") => {
            canvas.write_bmp(&options.output)
        }
        Some(extension) if extension.eq_ignore_ascii_case("png") => {
            canvas.write_png(&options.output)
        }
        _ => canvas.write_ppm(&options.output),
    };
    write_image
        .map_err(|error| format!("could not write {}: {error}", options.output.display()))?;
    println!("{}", layout.describe());
    println!(
        "painted {}x{} image with {} non-background pixels at {}",
        canvas.width(),
        canvas.height(),
        canvas.non_background_pixels(),
        options.output.display()
    );
    for log in host.browser_mut().console_logs() {
        println!("console: {log}");
    }
    Ok(())
}

// a frame here is one clock tick, a layout of what it changed and a paint; the last frame
// painted comes back for the image the run writes
fn report_frame_times(
    host: &mut EngineHost,
    frames: usize,
    profile: bool,
) -> Result<Option<ceri_engine::Frame>, String> {
    ceri_engine::profile::enable(profile);
    let viewport = host.viewport();
    let mut samples = Vec::with_capacity(frames);
    // per frame: the tick's script, a layout of what it changed, then the paint, whose own
    // layout finds the one just made in the cache
    let mut parts = [
        Vec::with_capacity(frames),
        Vec::with_capacity(frames),
        Vec::with_capacity(frames),
    ];
    let mut last = None;
    for _ in 0..frames {
        let started = std::time::Instant::now();
        host.input(InputEvent::AdvanceTime(16))
            .map_err(|error| error.to_string())?;
        let ticked = started.elapsed();
        host.browser()
            .layout(viewport)
            .map_err(|error| error.to_string())?;
        let laid_out = started.elapsed();
        if let Some(frame) = host.render().map_err(|error| error.to_string())? {
            last = Some(frame);
        }
        let rendered = started.elapsed();
        samples.push(rendered.as_secs_f64() * 1000.0);
        for (part, span) in parts
            .iter_mut()
            .zip([ticked, laid_out - ticked, rendered - laid_out])
        {
            part.push(span.as_secs_f64() * 1000.0);
        }
    }
    let median = |values: &mut Vec<f64>| {
        values.sort_by(|left, right| left.partial_cmp(right).expect("frame times are finite"));
        values[values.len() / 2]
    };
    let mean = samples.iter().sum::<f64>() / samples.len() as f64;
    let middle = median(&mut samples);
    let worst = samples[samples.len() - 1];
    println!(
        "{frames} frames: {middle:.2} ms median, {mean:.2} ms mean, {worst:.2} ms worst, {:.0} fps at the median",
        1000.0 / middle.max(f64::EPSILON)
    );
    let [script, layout, paint] = &mut parts;
    println!(
        "per frame: script {:.2} ms, layout {:.2} ms, paint {:.2} ms",
        median(script),
        median(layout),
        median(paint)
    );
    if profile {
        println!("{}", ceri_engine::profile::report(frames));
        ceri_engine::profile::enable(false);
    }
    // a layout of a document that has not changed shows what the layout cache costs
    let mut layout_samples = Vec::with_capacity(frames);
    for _ in 0..frames {
        let started = std::time::Instant::now();
        host.browser()
            .layout(viewport)
            .map_err(|error| error.to_string())?;
        layout_samples.push(started.elapsed().as_secs_f64() * 1000.0);
    }
    layout_samples.sort_by(|left, right| left.partial_cmp(right).expect("times are finite"));
    println!(
        "layout of an unchanged document: {:.2} ms median",
        layout_samples[layout_samples.len() / 2]
    );
    Ok(last)
}

fn load_font(path: &Path) -> Result<std::rc::Rc<FontFace>, String> {
    let bytes =
        fs::read(path).map_err(|error| format!("could not read {}: {error}", path.display()))?;
    FontFace::parse(bytes)
        .map(std::rc::Rc::new)
        .map_err(|error| format!("could not parse {}: {error}", path.display()))
}

fn parse_args<I>(args: I) -> Result<Options, String>
where
    I: IntoIterator<Item = std::ffi::OsString>,
{
    let mut input = None;
    let mut output = PathBuf::from("target/ceri-engine.ppm");
    let mut width = 800;
    let mut height = 520;
    let mut clicks = Vec::new();
    let mut click_points = Vec::new();
    let mut pointer_moves = Vec::new();
    let mut text_inputs = Vec::new();
    let mut keys = Vec::new();
    let mut url = format!("{APP_ORIGIN}/");
    let mut asset_root = None;
    let mut scale = 1.0_f32;
    let mut font = None;
    let mut bold_font = None;
    let mut frames = 0_usize;
    let mut profile = false;
    let mut ticks = Vec::new();
    let mut args = args.into_iter();
    while let Some(argument) = args.next() {
        let argument = argument
            .into_string()
            .map_err(|_| "arguments must be valid UTF-8".to_string())?;
        match argument.as_str() {
            "--input" => input = Some(PathBuf::from(next_value(&mut args, "--input")?)),
            "--output" => output = PathBuf::from(next_value(&mut args, "--output")?),
            "--width" => width = parse_dimension(next_value(&mut args, "--width")?, "width")?,
            "--height" => height = parse_dimension(next_value(&mut args, "--height")?, "height")?,
            "--click" => clicks.push(next_value(&mut args, "--click")?),
            "--click-at" => click_points.push(parse_point(next_value(&mut args, "--click-at")?)?),
            "--move-at" => pointer_moves.push(parse_point(next_value(&mut args, "--move-at")?)?),
            "--text" => text_inputs.push(next_value(&mut args, "--text")?),
            "--key" => keys.push(next_value(&mut args, "--key")?),
            "--url" => url = next_value(&mut args, "--url")?,
            "--asset-root" => {
                asset_root = Some(PathBuf::from(next_value(&mut args, "--asset-root")?))
            }
            "--scale" => scale = parse_scale(next_value(&mut args, "--scale")?)?,
            "--frames" => {
                frames = next_value(&mut args, "--frames")?
                    .parse()
                    .map_err(|_| "frames must be a non-negative integer".to_string())?
            }
            "--font" => font = Some(PathBuf::from(next_value(&mut args, "--font")?)),
            "--bold-font" => bold_font = Some(PathBuf::from(next_value(&mut args, "--bold-font")?)),
            "--tick" => ticks.push(parse_delay(next_value(&mut args, "--tick")?)?),
            "--profile" => profile = true,
            "--help" | "-h" => {
                println!(
                    "usage: ceri-engine --input FILE [--asset-root DIR] [--url URL] [--output FILE] [--width PX] [--height PX] [--scale RATIO] [--font FILE] [--bold-font FILE] [--frames N] [--profile] [--click ID] [--click-at X,Y] [--move-at X,Y] [--text TEXT] [--key KEY] [--tick MS]"
                );
                std::process::exit(0);
            }
            other => return Err(format!("unknown argument {other}")),
        }
    }
    let input = input.ok_or_else(|| "--input is required".to_string())?;
    Ok(Options {
        input,
        output,
        clicks,
        click_points,
        pointer_moves,
        text_inputs,
        keys,
        url,
        asset_root,
        ticks,
        scale,
        font,
        bold_font,
        frames,
        profile,
        viewport: Size {
            width: width as f32,
            height: height as f32,
        },
    })
}

fn next_value<I>(args: &mut I, flag: &str) -> Result<String, String>
where
    I: Iterator<Item = std::ffi::OsString>,
{
    args.next()
        .ok_or_else(|| format!("{flag} requires a value"))?
        .into_string()
        .map_err(|_| format!("{flag} value must be valid UTF-8"))
}

fn parse_dimension(value: String, name: &str) -> Result<u32, String> {
    let dimension = value
        .parse::<u32>()
        .map_err(|_| format!("{name} must be a positive integer"))?;
    if dimension == 0 {
        return Err(format!("{name} must be a positive integer"));
    }
    Ok(dimension)
}

fn parse_scale(value: String) -> Result<f32, String> {
    let scale = value
        .parse::<f32>()
        .map_err(|_| "scale must be a positive number".to_string())?;
    (scale.is_finite() && scale > 0.0)
        .then_some(scale)
        .ok_or_else(|| "scale must be a positive number".to_string())
}

fn parse_delay(value: String) -> Result<u64, String> {
    value
        .parse::<u64>()
        .map_err(|_| "tick must be a non-negative integer".to_string())
}

fn parse_point(value: String) -> Result<(f32, f32), String> {
    let (x, y) = value
        .split_once(',')
        .ok_or_else(|| "click-at must use X,Y coordinates".to_string())?;
    let x = parse_coordinate(x.trim(), "x")?;
    let y = parse_coordinate(y.trim(), "y")?;
    Ok((x, y))
}

fn parse_coordinate(value: &str, name: &str) -> Result<f32, String> {
    let coordinate = value
        .parse::<f32>()
        .map_err(|_| format!("{name} coordinate must be finite"))?;
    coordinate
        .is_finite()
        .then_some(coordinate)
        .ok_or_else(|| format!("{name} coordinate must be finite"))
}

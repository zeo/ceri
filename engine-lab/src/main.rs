use ceri_engine_lab::{APP_ORIGIN, Browser, ResourceBundle, Size};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

struct Options {
    input: PathBuf,
    output: PathBuf,
    viewport: Size,
    clicks: Vec<String>,
    click_points: Vec<(f32, f32)>,
    url: String,
    asset_root: Option<PathBuf>,
    ticks: Vec<u64>,
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
    let status = browser
        .navigate(&options.url)
        .map_err(|error| error.to_string())?;
    println!("navigation {}: {status}", options.url);
    for id in &options.clicks {
        let dispatched = browser.click_id(id).map_err(|error| error.to_string())?;
        println!(
            "click #{id}: {}",
            if dispatched { "handled" } else { "no listener" }
        );
    }
    for &(x, y) in &options.click_points {
        let dispatched = browser
            .click_at(options.viewport, x, y)
            .map_err(|error| error.to_string())?;
        println!(
            "click @{x},{y}: {}",
            if dispatched { "handled" } else { "no listener" }
        );
    }
    for milliseconds in &options.ticks {
        let executed = browser
            .advance_time(*milliseconds)
            .map_err(|error| error.to_string())?;
        println!("advanced {milliseconds}ms: {executed} task(s)");
    }
    let layout = browser
        .layout(options.viewport)
        .map_err(|error| error.to_string())?;
    let canvas = layout.paint();
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
    for log in browser.console_logs() {
        println!("console: {log}");
    }
    Ok(())
}

fn parse_args<I>(args: I) -> Result<Options, String>
where
    I: IntoIterator<Item = std::ffi::OsString>,
{
    let mut input = None;
    let mut output = PathBuf::from("target/ceri-engine-lab.ppm");
    let mut width = 800;
    let mut height = 520;
    let mut clicks = Vec::new();
    let mut click_points = Vec::new();
    let mut url = format!("{APP_ORIGIN}/");
    let mut asset_root = None;
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
            "--url" => url = next_value(&mut args, "--url")?,
            "--asset-root" => {
                asset_root = Some(PathBuf::from(next_value(&mut args, "--asset-root")?))
            }
            "--tick" => ticks.push(parse_delay(next_value(&mut args, "--tick")?)?),
            "--help" | "-h" => {
                println!(
                    "usage: ceri-engine-lab --input FILE [--asset-root DIR] [--url URL] [--output FILE] [--width PX] [--height PX] [--click ID] [--click-at X,Y] [--tick MS]"
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
        url,
        asset_root,
        ticks,
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

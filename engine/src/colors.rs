// CSS color values from CSS Color Level 4: hex, the named colors, rgb(), hsl() and hwb() in
// both syntaxes, lab(), lch(), oklab() and oklch(), currentColor, and color-mix() in the srgb,
// xyz and lab families; colors outside srgb clip per channel, as chromium draws them

use crate::css::Color;

// the named colors of CSS Color Level 4, sorted for binary search
const NAMED: &[(&str, u32)] = &[
    ("aliceblue", 0xf0f8ff),
    ("antiquewhite", 0xfaebd7),
    ("aqua", 0x00ffff),
    ("aquamarine", 0x7fffd4),
    ("azure", 0xf0ffff),
    ("beige", 0xf5f5dc),
    ("bisque", 0xffe4c4),
    ("black", 0x000000),
    ("blanchedalmond", 0xffebcd),
    ("blue", 0x0000ff),
    ("blueviolet", 0x8a2be2),
    ("brown", 0xa52a2a),
    ("burlywood", 0xdeb887),
    ("cadetblue", 0x5f9ea0),
    ("chartreuse", 0x7fff00),
    ("chocolate", 0xd2691e),
    ("coral", 0xff7f50),
    ("cornflowerblue", 0x6495ed),
    ("cornsilk", 0xfff8dc),
    ("crimson", 0xdc143c),
    ("cyan", 0x00ffff),
    ("darkblue", 0x00008b),
    ("darkcyan", 0x008b8b),
    ("darkgoldenrod", 0xb8860b),
    ("darkgray", 0xa9a9a9),
    ("darkgreen", 0x006400),
    ("darkgrey", 0xa9a9a9),
    ("darkkhaki", 0xbdb76b),
    ("darkmagenta", 0x8b008b),
    ("darkolivegreen", 0x556b2f),
    ("darkorange", 0xff8c00),
    ("darkorchid", 0x9932cc),
    ("darkred", 0x8b0000),
    ("darksalmon", 0xe9967a),
    ("darkseagreen", 0x8fbc8f),
    ("darkslateblue", 0x483d8b),
    ("darkslategray", 0x2f4f4f),
    ("darkslategrey", 0x2f4f4f),
    ("darkturquoise", 0x00ced1),
    ("darkviolet", 0x9400d3),
    ("deeppink", 0xff1493),
    ("deepskyblue", 0x00bfff),
    ("dimgray", 0x696969),
    ("dimgrey", 0x696969),
    ("dodgerblue", 0x1e90ff),
    ("firebrick", 0xb22222),
    ("floralwhite", 0xfffaf0),
    ("forestgreen", 0x228b22),
    ("fuchsia", 0xff00ff),
    ("gainsboro", 0xdcdcdc),
    ("ghostwhite", 0xf8f8ff),
    ("gold", 0xffd700),
    ("goldenrod", 0xdaa520),
    ("gray", 0x808080),
    ("green", 0x008000),
    ("greenyellow", 0xadff2f),
    ("grey", 0x808080),
    ("honeydew", 0xf0fff0),
    ("hotpink", 0xff69b4),
    ("indianred", 0xcd5c5c),
    ("indigo", 0x4b0082),
    ("ivory", 0xfffff0),
    ("khaki", 0xf0e68c),
    ("lavender", 0xe6e6fa),
    ("lavenderblush", 0xfff0f5),
    ("lawngreen", 0x7cfc00),
    ("lemonchiffon", 0xfffacd),
    ("lightblue", 0xadd8e6),
    ("lightcoral", 0xf08080),
    ("lightcyan", 0xe0ffff),
    ("lightgoldenrodyellow", 0xfafad2),
    ("lightgray", 0xd3d3d3),
    ("lightgreen", 0x90ee90),
    ("lightgrey", 0xd3d3d3),
    ("lightpink", 0xffb6c1),
    ("lightsalmon", 0xffa07a),
    ("lightseagreen", 0x20b2aa),
    ("lightskyblue", 0x87cefa),
    ("lightslategray", 0x778899),
    ("lightslategrey", 0x778899),
    ("lightsteelblue", 0xb0c4de),
    ("lightyellow", 0xffffe0),
    ("lime", 0x00ff00),
    ("limegreen", 0x32cd32),
    ("linen", 0xfaf0e6),
    ("magenta", 0xff00ff),
    ("maroon", 0x800000),
    ("mediumaquamarine", 0x66cdaa),
    ("mediumblue", 0x0000cd),
    ("mediumorchid", 0xba55d3),
    ("mediumpurple", 0x9370db),
    ("mediumseagreen", 0x3cb371),
    ("mediumslateblue", 0x7b68ee),
    ("mediumspringgreen", 0x00fa9a),
    ("mediumturquoise", 0x48d1cc),
    ("mediumvioletred", 0xc71585),
    ("midnightblue", 0x191970),
    ("mintcream", 0xf5fffa),
    ("mistyrose", 0xffe4e1),
    ("moccasin", 0xffe4b5),
    ("navajowhite", 0xffdead),
    ("navy", 0x000080),
    ("oldlace", 0xfdf5e6),
    ("olive", 0x808000),
    ("olivedrab", 0x6b8e23),
    ("orange", 0xffa500),
    ("orangered", 0xff4500),
    ("orchid", 0xda70d6),
    ("palegoldenrod", 0xeee8aa),
    ("palegreen", 0x98fb98),
    ("paleturquoise", 0xafeeee),
    ("palevioletred", 0xdb7093),
    ("papayawhip", 0xffefd5),
    ("peachpuff", 0xffdab9),
    ("peru", 0xcd853f),
    ("pink", 0xffc0cb),
    ("plum", 0xdda0dd),
    ("powderblue", 0xb0e0e6),
    ("purple", 0x800080),
    ("rebeccapurple", 0x663399),
    ("red", 0xff0000),
    ("rosybrown", 0xbc8f8f),
    ("royalblue", 0x4169e1),
    ("saddlebrown", 0x8b4513),
    ("salmon", 0xfa8072),
    ("sandybrown", 0xf4a460),
    ("seagreen", 0x2e8b57),
    ("seashell", 0xfff5ee),
    ("sienna", 0xa0522d),
    ("silver", 0xc0c0c0),
    ("skyblue", 0x87ceeb),
    ("slateblue", 0x6a5acd),
    ("slategray", 0x708090),
    ("slategrey", 0x708090),
    ("snow", 0xfffafa),
    ("springgreen", 0x00ff7f),
    ("steelblue", 0x4682b4),
    ("tan", 0xd2b48c),
    ("teal", 0x008080),
    ("thistle", 0xd8bfd8),
    ("tomato", 0xff6347),
    ("turquoise", 0x40e0d0),
    ("violet", 0xee82ee),
    ("wheat", 0xf5deb3),
    ("white", 0xffffff),
    ("whitesmoke", 0xf5f5f5),
    ("yellow", 0xffff00),
    ("yellowgreen", 0x9acd32),
];

fn named(name: &str) -> Option<Color> {
    let index = NAMED
        .binary_search_by(|(candidate, _)| candidate.cmp(&name))
        .ok()?;
    let rgb = NAMED[index].1;
    Some(Color::opaque(
        (rgb >> 16) as u8,
        (rgb >> 8) as u8,
        rgb as u8,
    ))
}

fn channel(value: f32) -> u8 {
    (value.clamp(0.0, 1.0) * 255.0).round() as u8
}

// the arguments of a color function, in the comma syntax or the space syntax with a slash
// before the alpha; `none` reads as zero
fn arguments(inner: &str) -> Option<(Vec<&str>, Option<&str>)> {
    if inner.contains(',') {
        let parts = inner.split(',').map(str::trim).collect::<Vec<_>>();
        return match parts.len() {
            3 => Some((parts, None)),
            4 => Some((parts[..3].to_vec(), Some(parts[3]))),
            _ => None,
        };
    }
    let (channels, alpha) = match inner.split_once('/') {
        Some((channels, alpha)) => (channels, Some(alpha.trim())),
        None => (inner, None),
    };
    let parts = channels.split_whitespace().collect::<Vec<_>>();
    (parts.len() == 3).then_some((parts, alpha))
}

fn number(value: &str) -> Option<f32> {
    if value == "none" {
        return Some(0.0);
    }
    value.parse::<f32>().ok().filter(|value| value.is_finite())
}

// a fraction from a number scaled against `full`, or a percentage
fn fraction(value: &str, full: f32) -> Option<f32> {
    match value.strip_suffix('%') {
        Some(percent) => number(percent).map(|percent| percent / 100.0),
        None => number(value).map(|value| value / full),
    }
}

fn alpha(value: Option<&str>) -> Option<f32> {
    value.map_or(Some(1.0), |value| fraction(value, 1.0))
}

// a hue in degrees from deg, rad, grad, turn or a bare number
fn hue(value: &str) -> Option<f32> {
    let (number_text, scale) = if let Some(text) = value.strip_suffix("deg") {
        (text, 1.0)
    } else if let Some(text) = value.strip_suffix("grad") {
        (text, 0.9)
    } else if let Some(text) = value.strip_suffix("rad") {
        (text, 180.0 / std::f32::consts::PI)
    } else if let Some(text) = value.strip_suffix("turn") {
        (text, 360.0)
    } else {
        (value, 1.0)
    };
    number(number_text).map(|degrees| (degrees * scale).rem_euclid(360.0))
}

fn hsl_to_rgb(hue: f32, saturation: f32, lightness: f32) -> [f32; 3] {
    let saturation = saturation.clamp(0.0, 1.0);
    let lightness = lightness.clamp(0.0, 1.0);
    let component = |offset: f32| {
        let k = (offset + hue / 30.0).rem_euclid(12.0);
        let a = saturation * lightness.min(1.0 - lightness);
        lightness - a * (k - 3.0).min(9.0 - k).clamp(-1.0, 1.0)
    };
    [component(0.0), component(8.0), component(4.0)]
}

fn rgb_function(inner: &str) -> Option<Color> {
    let (channels, alpha_text) = arguments(inner)?;
    let mut values = [0.0; 3];
    for (slot, value) in values.iter_mut().zip(&channels) {
        *slot = fraction(value, 255.0)?;
    }
    Some(Color::rgba(
        channel(values[0]),
        channel(values[1]),
        channel(values[2]),
        channel(alpha(alpha_text)?),
    ))
}

fn hsl_function(inner: &str) -> Option<Color> {
    let (channels, alpha_text) = arguments(inner)?;
    let [red, green, blue] = hsl_to_rgb(
        hue(channels[0])?,
        fraction(channels[1], 100.0)?,
        fraction(channels[2], 100.0)?,
    );
    Some(Color::rgba(
        channel(red),
        channel(green),
        channel(blue),
        channel(alpha(alpha_text)?),
    ))
}

fn hwb_function(inner: &str) -> Option<Color> {
    let (channels, alpha_text) = arguments(inner)?;
    let hue = hue(channels[0])?;
    let mut white = fraction(channels[1], 100.0)?.clamp(0.0, 1.0);
    let mut black = fraction(channels[2], 100.0)?.clamp(0.0, 1.0);
    if white + black >= 1.0 {
        let total = white + black;
        white /= total;
        black /= total;
    }
    let base = hsl_to_rgb(hue, 1.0, 0.5);
    let mix = |value: f32| value * (1.0 - white - black) + white;
    Some(Color::rgba(
        channel(mix(base[0])),
        channel(mix(base[1])),
        channel(mix(base[2])),
        channel(alpha(alpha_text)?),
    ))
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Space {
    Srgb,
    SrgbLinear,
    XyzD65,
    XyzD50,
    Lab,
    Lch,
    Oklab,
    Oklch,
}

// what each channel means, so a channel left out of one color can be found in another
#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Red,
    Green,
    Blue,
    Lightness,
    OpponentA,
    OpponentB,
    Chroma,
    Hue,
}

impl Space {
    fn named(name: &str) -> Option<Self> {
        Some(match name {
            "srgb" => Self::Srgb,
            "srgb-linear" => Self::SrgbLinear,
            "xyz" | "xyz-d65" => Self::XyzD65,
            "xyz-d50" => Self::XyzD50,
            "lab" => Self::Lab,
            "lch" => Self::Lch,
            "oklab" => Self::Oklab,
            "oklch" => Self::Oklch,
            _ => return None,
        })
    }

    fn polar(self) -> bool {
        matches!(self, Self::Lch | Self::Oklch)
    }

    fn kinds(self) -> [Kind; 3] {
        match self {
            Self::Srgb | Self::SrgbLinear | Self::XyzD65 | Self::XyzD50 => {
                [Kind::Red, Kind::Green, Kind::Blue]
            }
            Self::Lab | Self::Oklab => [Kind::Lightness, Kind::OpponentA, Kind::OpponentB],
            Self::Lch | Self::Oklch => [Kind::Lightness, Kind::Chroma, Kind::Hue],
        }
    }
}

type Matrix = [[f64; 3]; 3];

// the matrices of the css color 4 sample code
const LINEAR_SRGB_TO_XYZ: Matrix = [
    [
        0.412_390_799_265_959_3,
        0.357_584_339_383_878,
        0.180_480_788_401_834_3,
    ],
    [
        0.212_639_005_871_510_27,
        0.715_168_678_767_756,
        0.072_192_315_360_733_71,
    ],
    [
        0.019_330_818_715_591_82,
        0.119_194_779_794_625_98,
        0.950_532_152_249_660_7,
    ],
];
const XYZ_TO_LINEAR_SRGB: Matrix = [
    [
        3.240_969_941_904_522_6,
        -1.537_383_177_570_094,
        -0.498_610_760_293_003_4,
    ],
    [
        -0.969_243_636_280_879_6,
        1.875_967_501_507_720_2,
        0.041_555_057_407_175_59,
    ],
    [
        0.055_630_079_696_993_66,
        -0.203_976_958_888_976_52,
        1.056_971_514_242_878_6,
    ],
];
const D65_TO_D50: Matrix = [
    [
        1.047_929_820_840_548_8,
        0.022_946_793_341_019_088,
        -0.050_192_229_543_135_57,
    ],
    [
        0.029_627_815_688_159_344,
        0.990_434_484_573_249,
        -0.017_073_825_029_385_14,
    ],
    [
        -0.009_243_058_152_591_178,
        0.015_055_144_896_577_895,
        0.751_874_289_958_000_8,
    ],
];
const D50_TO_D65: Matrix = [
    [
        0.955_473_452_704_218_2,
        -0.023_098_536_874_261_423,
        0.063_259_308_661_021_7,
    ],
    [
        -0.028_369_706_963_208_136,
        1.009_995_458_005_822_6,
        0.021_041_398_966_943_008,
    ],
    [
        0.012_314_001_688_319_899,
        -0.020_507_696_433_477_912,
        1.330_365_936_608_075_3,
    ],
];
const XYZ_TO_LMS: Matrix = [
    [
        0.819_022_437_996_703,
        0.361_906_260_052_890_4,
        -0.128_873_781_520_987_9,
    ],
    [
        0.032_983_653_932_388_5,
        0.929_286_861_586_343_4,
        0.036_144_666_350_642_4,
    ],
    [
        0.048_177_189_359_624_2,
        0.264_239_531_752_730_8,
        0.633_547_828_469_430_9,
    ],
];
const LMS_TO_OKLAB: Matrix = [
    [
        0.210_454_268_309_314,
        0.793_617_774_702_305_4,
        -0.004_072_043_011_619_3,
    ],
    [
        1.977_998_532_431_168_4,
        -2.428_592_242_048_58,
        0.450_593_709_617_411,
    ],
    [
        0.025_904_042_465_547_8,
        0.782_771_712_457_529_6,
        -0.808_675_754_923_077_4,
    ],
];
const OKLAB_TO_LMS: Matrix = [
    [1.0, 0.396_337_777_376_174_9, 0.215_803_757_309_913_6],
    [1.0, -0.105_561_345_815_658_6, -0.063_854_172_825_813_3],
    [1.0, -0.089_484_177_529_811_9, -1.291_485_548_019_409_2],
];
const LMS_TO_XYZ: Matrix = [
    [
        1.226_879_875_845_924_3,
        -0.557_814_994_460_217_1,
        0.281_391_045_665_964_7,
    ],
    [
        -0.040_575_745_214_800_8,
        1.112_286_803_280_317,
        -0.071_611_058_065_516_4,
    ],
    [
        -0.076_372_936_674_660_1,
        -0.421_493_332_402_243_2,
        1.586_924_019_836_781_6,
    ],
];
const D50_WHITE: [f64; 3] = [0.3457 / 0.3585, 1.0, (1.0 - 0.3457 - 0.3585) / 0.3585];
const LAB_EPSILON: f64 = 216.0 / 24389.0;
const LAB_KAPPA: f64 = 24389.0 / 27.0;

fn transform(matrix: &Matrix, value: [f64; 3]) -> [f64; 3] {
    matrix.map(|row| row[0] * value[0] + row[1] * value[1] + row[2] * value[2])
}

// the srgb transfer curve, mirrored for negative values
fn to_linear(value: f64) -> f64 {
    if value.abs() <= 0.04045 {
        value / 12.92
    } else {
        value.signum() * ((value.abs() + 0.055) / 1.055).powf(2.4)
    }
}

fn from_linear(value: f64) -> f64 {
    if value.abs() <= 0.003_130_8 {
        value * 12.92
    } else {
        value.signum() * (1.055 * value.abs().powf(1.0 / 2.4) - 0.055)
    }
}

fn to_polar([lightness, a, b]: [f64; 3]) -> [f64; 3] {
    [
        lightness,
        a.hypot(b),
        b.atan2(a).to_degrees().rem_euclid(360.0),
    ]
}

fn from_polar([lightness, chroma, hue]: [f64; 3]) -> [f64; 3] {
    let (sin, cos) = hue.to_radians().sin_cos();
    [lightness, chroma * cos, chroma * sin]
}

fn lab_to_xyz([lightness, a, b]: [f64; 3]) -> [f64; 3] {
    let f1 = (lightness + 16.0) / 116.0;
    let f0 = a / 500.0 + f1;
    let f2 = f1 - b / 200.0;
    let cube = |f: f64| {
        if f.powi(3) > LAB_EPSILON {
            f.powi(3)
        } else {
            (116.0 * f - 16.0) / LAB_KAPPA
        }
    };
    let y = if lightness > LAB_KAPPA * LAB_EPSILON {
        f1.powi(3)
    } else {
        lightness / LAB_KAPPA
    };
    let d50 = [cube(f0) * D50_WHITE[0], y, cube(f2) * D50_WHITE[2]];
    transform(&D50_TO_D65, d50)
}

fn xyz_to_lab(xyz: [f64; 3]) -> [f64; 3] {
    let d50 = transform(&D65_TO_D50, xyz);
    let f = |index: usize| {
        let value = d50[index] / D50_WHITE[index];
        if value > LAB_EPSILON {
            value.cbrt()
        } else {
            (LAB_KAPPA * value + 16.0) / 116.0
        }
    };
    let (x, y, z) = (f(0), f(1), f(2));
    [116.0 * y - 16.0, 500.0 * (x - y), 200.0 * (y - z)]
}

fn oklab_to_xyz(oklab: [f64; 3]) -> [f64; 3] {
    transform(
        &LMS_TO_XYZ,
        transform(&OKLAB_TO_LMS, oklab).map(|value| value.powi(3)),
    )
}

fn xyz_to_oklab(xyz: [f64; 3]) -> [f64; 3] {
    transform(&LMS_TO_OKLAB, transform(&XYZ_TO_LMS, xyz).map(f64::cbrt))
}

// d65 xyz, which every space converts through
fn to_xyz(space: Space, value: [f64; 3]) -> [f64; 3] {
    match space {
        Space::Srgb => transform(&LINEAR_SRGB_TO_XYZ, value.map(to_linear)),
        Space::SrgbLinear => transform(&LINEAR_SRGB_TO_XYZ, value),
        Space::XyzD65 => value,
        Space::XyzD50 => transform(&D50_TO_D65, value),
        Space::Lab => lab_to_xyz(value),
        Space::Lch => lab_to_xyz(from_polar(value)),
        Space::Oklab => oklab_to_xyz(value),
        Space::Oklch => oklab_to_xyz(from_polar(value)),
    }
}

fn from_xyz(space: Space, xyz: [f64; 3]) -> [f64; 3] {
    match space {
        Space::Srgb => transform(&XYZ_TO_LINEAR_SRGB, xyz).map(from_linear),
        Space::SrgbLinear => transform(&XYZ_TO_LINEAR_SRGB, xyz),
        Space::XyzD65 => xyz,
        Space::XyzD50 => transform(&D65_TO_D50, xyz),
        Space::Lab => xyz_to_lab(xyz),
        Space::Lch => to_polar(xyz_to_lab(xyz)),
        Space::Oklab => xyz_to_oklab(xyz),
        Space::Oklch => to_polar(xyz_to_oklab(xyz)),
    }
}

// a color in its own space at full precision, with the channels written as none still missing
#[derive(Clone, Copy, Debug)]
struct Components {
    space: Space,
    values: [Option<f64>; 3],
    alpha: f64,
}

impl Components {
    fn of(color: Color) -> Self {
        Self {
            space: Space::Srgb,
            values: [color.red, color.green, color.blue]
                .map(|value| Some(f64::from(value) / 255.0)),
            alpha: f64::from(color.alpha) / 255.0,
        }
    }

    fn color(self) -> Color {
        let srgb = from_xyz(
            Space::Srgb,
            to_xyz(self.space, self.values.map(|value| value.unwrap_or(0.0))),
        );
        Color::rgba(
            channel(srgb[0] as f32),
            channel(srgb[1] as f32),
            channel(srgb[2] as f32),
            channel(self.alpha as f32),
        )
    }

    // a missing channel stays missing where the target space has one of the same kind, and a
    // hue without chroma to show it goes missing too
    fn convert(self, target: Space) -> Self {
        let mut values = if self.space == target {
            self.values
        } else {
            let xyz = to_xyz(self.space, self.values.map(|value| value.unwrap_or(0.0)));
            let mut values = from_xyz(target, xyz).map(Some);
            for (value, kind) in self.values.iter().zip(self.space.kinds()) {
                if value.is_none()
                    && let Some(index) = target.kinds().iter().position(|other| *other == kind)
                {
                    values[index] = None;
                }
            }
            values
        };
        if target.polar() && values[1].is_some_and(|chroma| chroma.abs() < 1e-4) {
            values[2] = None;
        }
        Self {
            space: target,
            values,
            ..self
        }
    }
}

fn lab_family(name: &str) -> Option<Space> {
    Some(match name {
        "lab" => Space::Lab,
        "lch" => Space::Lch,
        "oklab" => Space::Oklab,
        "oklch" => Space::Oklch,
        _ => return None,
    })
}

// lab(), lch(), oklab() and oklch() take the space syntax only; percentages scale against
// the reference ranges of each channel
fn lab_function(space: Space, inner: &str) -> Option<Components> {
    if inner.contains(',') {
        return None;
    }
    let (channels, alpha_text) = arguments(inner)?;
    let (lightness, chroma, opponent) = match space {
        Space::Lab | Space::Lch => (100.0, 150.0, 125.0),
        _ => (1.0, 0.4, 0.4),
    };
    let scaled = |text: &str, full: f32| -> Option<Option<f64>> {
        if text == "none" {
            return Some(None);
        }
        Some(Some(f64::from(match text.strip_suffix('%') {
            Some(percent) => number(percent)? / 100.0 * full,
            None => number(text)?,
        })))
    };
    let mut values = [
        scaled(channels[0], lightness)?.map(|value| value.clamp(0.0, f64::from(lightness))),
        None,
        None,
    ];
    if space.polar() {
        values[1] = scaled(channels[1], chroma)?.map(|value| value.max(0.0));
        values[2] = if channels[2] == "none" {
            None
        } else {
            Some(f64::from(hue(channels[2])?))
        };
    } else {
        values[1] = scaled(channels[1], opponent)?;
        values[2] = scaled(channels[2], opponent)?;
    }
    Some(Components {
        space,
        values,
        alpha: f64::from(alpha(alpha_text)?.clamp(0.0, 1.0)),
    })
}

// the name and arguments of a function value
fn function(text: &str) -> Option<(&str, &str)> {
    let open = text.find('(')?;
    Some((
        text[..open].trim(),
        text[open + 1..].strip_suffix(')')?.trim(),
    ))
}

// a mix input at full precision when it is written in a lab space, else as parsed
fn mix_input(text: &str, current: Color) -> Option<Components> {
    let lower = text.trim().to_ascii_lowercase();
    if let Some((name, inner)) = function(&lower)
        && let Some(space) = lab_family(name)
    {
        return lab_function(space, inner);
    }
    Some(Components::of(parse(text, current)?))
}

// splits on commas that are not inside parentheses
fn top_level_commas(text: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth = 0;
    let mut start = 0;
    for (index, character) in text.char_indices() {
        match character {
            '(' => depth += 1,
            ')' => depth -= 1,
            ',' if depth == 0 => {
                parts.push(text[start..index].trim());
                start = index + 1;
            }
            _ => {}
        }
    }
    parts.push(text[start..].trim());
    parts
}

// a color and an optional percentage, in either order
fn mix_component(text: &str, current: Color) -> Option<(Components, Option<f32>)> {
    let text = text.trim();
    if let Some(split) = text.rfind(|character: char| character.is_whitespace())
        && let Some(percent) = text[split + 1..].strip_suffix('%')
        && let Some(percent) = number(percent)
    {
        return Some((mix_input(&text[..split], current)?, Some(percent / 100.0)));
    }
    if let Some(split) = text.find(|character: char| character.is_whitespace())
        && let Some(percent) = text[..split].strip_suffix('%')
        && let Some(percent) = number(percent)
    {
        return Some((
            mix_input(&text[split + 1..], current)?,
            Some(percent / 100.0),
        ));
    }
    Some((mix_input(text, current)?, None))
}

#[derive(Clone, Copy)]
enum HueArc {
    Shorter,
    Longer,
    Increasing,
    Decreasing,
}

impl HueArc {
    fn named(name: &str) -> Option<Self> {
        Some(match name {
            "shorter" => Self::Shorter,
            "longer" => Self::Longer,
            "increasing" => Self::Increasing,
            "decreasing" => Self::Decreasing,
            _ => return None,
        })
    }

    // shifts one hue by a turn so going from the first to the second takes this arc
    fn unwrap(self, from: f64, to: f64) -> (f64, f64) {
        let delta = to - from;
        match self {
            Self::Shorter if delta > 180.0 => (from + 360.0, to),
            Self::Shorter if delta < -180.0 => (from, to + 360.0),
            Self::Longer if delta > 0.0 && delta < 180.0 => (from + 360.0, to),
            Self::Longer if delta > -180.0 && delta <= 0.0 => (from, to + 360.0),
            Self::Increasing if delta < 0.0 => (from, to + 360.0),
            Self::Decreasing if delta > 0.0 => (from + 360.0, to),
            _ => (from, to),
        }
    }
}

fn color_mix(inner: &str, current: Color) -> Option<Color> {
    let parts = top_level_commas(inner);
    let [method, first, second] = parts.as_slice() else {
        return None;
    };
    let mut method = method.strip_prefix("in")?.split_whitespace();
    let space = Space::named(method.next()?)?;
    let arc = match (method.next(), method.next(), method.next()) {
        (None, ..) => HueArc::Shorter,
        (Some(arc), Some("hue"), None) if space.polar() => HueArc::named(arc)?,
        _ => return None,
    };
    let (first, first_share) = mix_component(first, current)?;
    let (second, second_share) = mix_component(second, current)?;
    let (first_share, second_share) = match (first_share, second_share) {
        (None, None) => (0.5, 0.5),
        (Some(share), None) => (share, 1.0 - share),
        (None, Some(share)) => (1.0 - share, share),
        (Some(first), Some(second)) => (first, second),
    };
    let total = first_share + second_share;
    if total <= 0.0 {
        return None;
    }
    // a total under 100% leaves the result partly transparent
    let scale = f64::from(total.min(1.0));
    let (first_share, second_share) = (
        f64::from(first_share / total),
        f64::from(second_share / total),
    );
    let (first, second) = (first.convert(space), second.convert(space));
    let alpha = first.alpha * first_share + second.alpha * second_share;
    let mut values = [None; 3];
    for (index, value) in values.iter_mut().enumerate() {
        // a channel missing from one color takes the other's
        let (from, to) = match (first.values[index], second.values[index]) {
            (Some(from), Some(to)) => (from, to),
            (Some(only), None) | (None, Some(only)) => (only, only),
            (None, None) => continue,
        };
        *value = Some(if space.polar() && index == 2 {
            let (from, to) = arc.unwrap(from, to);
            from * first_share + to * second_share
        } else if alpha == 0.0 {
            0.0
        } else {
            // channels mix premultiplied by alpha, as CSS Color 4 interpolation does
            (from * first.alpha * first_share + to * second.alpha * second_share) / alpha
        });
    }
    Some(
        Components {
            space,
            values,
            alpha: alpha * scale,
        }
        .color(),
    )
}

fn hex(digits: &str) -> Option<Color> {
    let nibble = |index: usize| -> Option<u8> {
        let digit = char::from(*digits.as_bytes().get(index)?).to_digit(16)? as u8;
        Some(digit * 17)
    };
    let byte = |index: usize| u8::from_str_radix(digits.get(index..index + 2)?, 16).ok();
    match digits.len() {
        3 => Some(Color::opaque(nibble(0)?, nibble(1)?, nibble(2)?)),
        4 => Some(Color::rgba(nibble(0)?, nibble(1)?, nibble(2)?, nibble(3)?)),
        6 => Some(Color::opaque(byte(0)?, byte(2)?, byte(4)?)),
        8 => Some(Color::rgba(byte(0)?, byte(2)?, byte(4)?, byte(6)?)),
        _ => None,
    }
}

// a CSS color, with currentColor resolving to the element's color
pub(crate) fn parse(input: &str, current: Color) -> Option<Color> {
    let input = input.trim().to_ascii_lowercase();
    if let Some(digits) = input.strip_prefix('#') {
        return hex(digits);
    }
    match input.as_str() {
        "transparent" => return Some(Color::transparent()),
        "currentcolor" => return Some(current),
        _ => {}
    }
    if input.contains('(') {
        let (name, inner) = function(&input)?;
        return match name {
            "rgb" | "rgba" => rgb_function(inner),
            "hsl" | "hsla" => hsl_function(inner),
            "hwb" => hwb_function(inner),
            "color-mix" => color_mix(inner, current),
            _ => lab_function(lab_family(name)?, inner).map(Components::color),
        };
    }
    named(&input)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_named_table_is_sorted_for_binary_search() {
        assert!(NAMED.windows(2).all(|pair| pair[0].0 < pair[1].0));
        assert_eq!(NAMED.len(), 148);
    }

    #[test]
    fn parses_every_color_syntax() {
        let current = Color::opaque(1, 2, 3);
        let cases = [
            ("#0f8", Color::opaque(0, 255, 136)),
            ("#11223344", Color::rgba(17, 34, 51, 68)),
            ("RebeccaPurple", Color::opaque(102, 51, 153)),
            ("currentColor", current),
            ("rgb(255, 0, 128)", Color::opaque(255, 0, 128)),
            ("rgba(255, 0, 0, 0.5)", Color::rgba(255, 0, 0, 128)),
            ("rgb(100% 0% 50% / 25%)", Color::rgba(255, 0, 128, 64)),
            ("rgb(none 10 20)", Color::opaque(0, 10, 20)),
            ("hsl(120, 100%, 50%)", Color::opaque(0, 255, 0)),
            ("hsl(0.5turn 100% 25% / 0.5)", Color::rgba(0, 128, 128, 128)),
            ("hwb(0 0% 0%)", Color::opaque(255, 0, 0)),
            ("hwb(0 50% 50%)", Color::opaque(128, 128, 128)),
            ("color-mix(in srgb, red, blue)", Color::opaque(128, 0, 128)),
            (
                "color-mix(in srgb, red 25%, white)",
                Color::opaque(255, 191, 191),
            ),
            (
                "color-mix(in srgb, currentColor 50%, transparent)",
                Color::rgba(1, 2, 3, 128),
            ),
        ];
        for (input, expected) in cases {
            assert_eq!(parse(input, current), Some(expected), "{input}");
        }
        assert_eq!(parse("rgb(1, 2)", current), None);
        assert_eq!(parse("color-mix(in hsl, red, blue)", current), None);
        assert_eq!(parse("lab(50, 40, -20)", current), None);
        assert_eq!(
            parse("color-mix(in lab longer hue, red, blue)", current),
            None
        );
        assert_eq!(parse("notacolor", current), None);
    }

    #[test]
    fn lab_spaces_and_their_mixes_land_where_chromium_draws_them() {
        // each expected color was read from a swatch edge drew in software
        let cases = [
            ("oklch(70% 0.1 200)", Color::opaque(64, 177, 183)),
            ("oklch(0.5 0.2 30)", Color::opaque(186, 13, 1)),
            ("oklch(90% 0.4 140)", Color::opaque(0, 255, 0)),
            ("oklab(0.6 0.1 -0.1)", Color::opaque(159, 99, 186)),
            ("oklab(50% 40% 0%)", Color::opaque(166, 47, 95)),
            ("lab(50 40 -20)", Color::opaque(171, 90, 154)),
            ("lab(80% 100 50)", Color::opaque(255, 73, 114)),
            ("lch(60 50 120)", Color::opaque(118, 156, 62)),
            ("lch(30% 80 270)", Color::opaque(0, 77, 200)),
            ("lch(50% 100% 60)", Color::opaque(236, 5, 0)),
            ("OKLCH(0.5 0.1 90deg)", Color::opaque(121, 96, 6)),
            ("oklch(0.7 none 50)", Color::opaque(158, 158, 158)),
            ("lab(150 0 0)", Color::opaque(255, 255, 255)),
            ("lab(50 40 -20 / 0.25)", Color::rgba(171, 90, 154, 64)),
            (
                "color-mix(in oklab, red, blue)",
                Color::opaque(140, 83, 162),
            ),
            ("color-mix(in oklch, red, blue)", Color::opaque(186, 0, 194)),
            ("color-mix(in lab, red, blue)", Color::opaque(193, 0, 136)),
            ("color-mix(in lch, red, blue)", Color::opaque(245, 0, 134)),
            ("color-mix(in xyz, red, blue)", Color::opaque(188, 0, 188)),
            (
                "color-mix(in xyz-d50, red, blue)",
                Color::opaque(188, 0, 188),
            ),
            (
                "color-mix(in oklch longer hue, red, blue)",
                Color::opaque(0, 147, 0),
            ),
            (
                "color-mix(in oklch increasing hue, red, blue)",
                Color::opaque(0, 147, 0),
            ),
            (
                "color-mix(in oklch decreasing hue, red, blue)",
                Color::opaque(186, 0, 194),
            ),
            // white and gray have no hue of their own, so they take the other color's
            (
                "color-mix(in oklch, white, blue)",
                Color::opaque(115, 163, 255),
            ),
            (
                "color-mix(in lch, #808080, red 30%)",
                Color::opaque(175, 112, 94),
            ),
            (
                "color-mix(in oklch, oklch(0.6 0.2 none), oklch(0.8 0.1 120))",
                Color::opaque(147, 171, 44),
            ),
            (
                "color-mix(in oklab, red 40%, transparent)",
                Color::rgba(255, 0, 0, 102),
            ),
            (
                "color-mix(in lch, red 20%, lime 20%)",
                Color::rgba(209, 169, 0, 102),
            ),
        ];
        for (input, expected) in cases {
            assert_eq!(parse(input, Color::BLACK), Some(expected), "{input}");
        }
    }
}

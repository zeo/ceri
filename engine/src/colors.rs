// CSS color values from CSS Color Level 4: hex, the named colors, rgb(), hsl() and hwb() in
// both syntaxes, currentColor, and color-mix() in the sRGB spaces

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

fn to_linear(value: u8) -> f32 {
    let value = f32::from(value) / 255.0;
    if value <= 0.04045 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    }
}

fn from_linear(value: f32) -> u8 {
    let value = if value <= 0.003_130_8 {
        value * 12.92
    } else {
        1.055 * value.powf(1.0 / 2.4) - 0.055
    };
    channel(value)
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
fn mix_component(text: &str, current: Color) -> Option<(Color, Option<f32>)> {
    let text = text.trim();
    if let Some(split) = text.rfind(|character: char| character.is_whitespace())
        && let Some(percent) = text[split + 1..].strip_suffix('%')
        && let Some(percent) = number(percent)
    {
        return Some((parse(&text[..split], current)?, Some(percent / 100.0)));
    }
    if let Some(split) = text.find(|character: char| character.is_whitespace())
        && let Some(percent) = text[..split].strip_suffix('%')
        && let Some(percent) = number(percent)
    {
        return Some((parse(&text[split + 1..], current)?, Some(percent / 100.0)));
    }
    Some((parse(text, current)?, None))
}

fn color_mix(inner: &str, current: Color) -> Option<Color> {
    let parts = top_level_commas(inner);
    let [space, first, second] = parts.as_slice() else {
        return None;
    };
    let linear = match space.strip_prefix("in")?.trim() {
        "srgb" => false,
        "srgb-linear" => true,
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
    let scale = total.min(1.0);
    let (first_share, second_share) = (first_share / total, second_share / total);
    let alpha_first = f32::from(first.alpha) / 255.0;
    let alpha_second = f32::from(second.alpha) / 255.0;
    let alpha = alpha_first * first_share + alpha_second * second_share;
    // channels mix premultiplied by alpha, as CSS Color 4 interpolation does
    let mix = |a: u8, b: u8| -> u8 {
        if alpha == 0.0 {
            return 0;
        }
        if linear {
            let value = (to_linear(a) * alpha_first * first_share
                + to_linear(b) * alpha_second * second_share)
                / alpha;
            from_linear(value)
        } else {
            let value = (f32::from(a) / 255.0 * alpha_first * first_share
                + f32::from(b) / 255.0 * alpha_second * second_share)
                / alpha;
            channel(value)
        }
    };
    Some(Color::rgba(
        mix(first.red, second.red),
        mix(first.green, second.green),
        mix(first.blue, second.blue),
        channel(alpha * scale),
    ))
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
    if let Some(open) = input.find('(') {
        let name = input[..open].trim();
        let inner = input[open + 1..].strip_suffix(')')?.trim();
        return match name {
            "rgb" | "rgba" => rgb_function(inner),
            "hsl" | "hsla" => hsl_function(inner),
            "hwb" => hwb_function(inner),
            "color-mix" => color_mix(inner, current),
            _ => None,
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
        assert_eq!(parse("color-mix(in lab, red, blue)", current), None);
        assert_eq!(parse("notacolor", current), None);
    }
}

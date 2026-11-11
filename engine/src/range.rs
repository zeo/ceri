use crate::dom::Element;

pub(crate) fn is_range(element: &Element) -> bool {
    element.name == "input"
        && element
            .attr("type")
            .is_some_and(|kind| kind.trim().eq_ignore_ascii_case("range"))
}

// the min, max and step of an input type=range, by the html value sanitization and step rules
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct RangeBounds {
    pub(crate) min: f64,
    pub(crate) max: f64,
    // none for step="any"
    step: Option<f64>,
    base: f64,
    // decimal places of the step and its base, so snapped values print without float noise
    places: i32,
}

impl RangeBounds {
    pub(crate) fn of(element: &Element) -> Self {
        let number = |name: &str| element.attr(name).and_then(parse_float);
        let min = number("min").unwrap_or(0.0);
        let max = number("max").unwrap_or(100.0).max(min);
        let step_text = element.attr("step").map(str::trim);
        let step = if step_text.is_some_and(|step| step.eq_ignore_ascii_case("any")) {
            None
        } else {
            Some(
                step_text
                    .and_then(parse_float)
                    .filter(|step| *step > 0.0)
                    .unwrap_or(1.0),
            )
        };
        let base_text = element
            .attr("min")
            .filter(|text| parse_float(text).is_some())
            .or_else(|| {
                element
                    .attr("value")
                    .filter(|text| parse_float(text).is_some())
            });
        let places = [step_text, base_text]
            .into_iter()
            .flatten()
            .map(decimal_places)
            .max()
            .unwrap_or(0);
        Self {
            min,
            max,
            step,
            base: base_text.and_then(parse_float).unwrap_or(0.0),
            places,
        }
    }

    // a value in range on the step grid, rounding a midpoint up
    pub(crate) fn clamp(&self, value: f64) -> f64 {
        let clamped = value.clamp(self.min, self.max);
        let Some(step) = self.step else {
            return clamped;
        };
        let snapped = self.base + ((clamped - self.base) / step + 0.5).floor() * step;
        let snapped = if snapped > self.max {
            snapped - step
        } else if snapped < self.min {
            snapped + step
        } else {
            snapped
        };
        let scale = 10f64.powi(self.places);
        (snapped * scale).round() / scale
    }

    // the value an unparsable or missing value attribute stands for
    pub(crate) fn sanitize(&self, raw: Option<&str>) -> f64 {
        self.clamp(
            raw.and_then(parse_float)
                .unwrap_or(self.min + (self.max - self.min) / 2.0),
        )
    }

    pub(crate) fn fraction(&self, value: f64) -> f64 {
        if self.max > self.min {
            ((value - self.min) / (self.max - self.min)).clamp(0.0, 1.0)
        } else {
            0.0
        }
    }

    pub(crate) fn at_fraction(&self, fraction: f64) -> f64 {
        self.clamp(self.min + fraction.clamp(0.0, 1.0) * (self.max - self.min))
    }

    // where a key moves the value, as chromium steps a focused slider
    pub(crate) fn after_key(&self, value: f64, key: &str) -> Option<f64> {
        let step = self.step.unwrap_or((self.max - self.min) / 100.0);
        let page = ((self.max - self.min) / 10.0).max(step);
        let next = match key {
            "ArrowUp" | "ArrowRight" => value + step,
            "ArrowDown" | "ArrowLeft" => value - step,
            "PageUp" => value + page,
            "PageDown" => value - page,
            "Home" => self.min,
            "End" => self.max,
            _ => return None,
        };
        Some(self.clamp(next))
    }
}

pub(crate) fn serialize(value: f64) -> String {
    // negative zero prints as 0, as a JS number does
    format!("{}", value + 0.0)
}

// a valid floating-point number in the html sense: no leading plus, whitespace or bare dot
fn parse_float(text: &str) -> Option<f64> {
    let valid = text
        .bytes()
        .all(|byte| byte.is_ascii_digit() || matches!(byte, b'.' | b'-' | b'+' | b'e' | b'E'))
        && !text.starts_with('+')
        && !text.ends_with('.')
        && text.bytes().any(|byte| byte.is_ascii_digit());
    text.parse::<f64>()
        .ok()
        .filter(|value| valid && value.is_finite())
}

fn decimal_places(text: &str) -> i32 {
    let (mantissa, exponent) = text
        .split_once(['e', 'E'])
        .map_or((text, 0), |(mantissa, exponent)| {
            (mantissa, exponent.parse::<i32>().unwrap_or(0))
        });
    let places = mantissa
        .split_once('.')
        .map_or(0, |(_, fraction)| fraction.len() as i32);
    (places - exponent).clamp(0, 15)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dom::Document;

    fn bounds(attributes: &str) -> RangeBounds {
        let document = Document::parse(&format!("<input type=range {attributes}>"))
            .expect("range input should parse");
        let node = document.find_elements_by_selector("input").unwrap()[0];
        RangeBounds::of(document.element(node).unwrap())
    }

    #[test]
    fn values_snap_to_the_step_from_the_min() {
        let range = bounds("min=1 max=9 step=3");
        assert_eq!(range.sanitize(Some("5")), 4.0);
        assert_eq!(range.sanitize(Some("5.5")), 7.0);
        // the top of the range is off the grid, so the value settles on the last step below it
        assert_eq!(range.sanitize(Some("100")), 7.0);
        assert_eq!(range.sanitize(Some("-4")), 1.0);
        assert_eq!(range.sanitize(Some("x")), 4.0);
        assert_eq!(range.sanitize(None), 4.0);
    }

    #[test]
    fn defaults_follow_the_html_rules() {
        let range = bounds("");
        assert_eq!((range.min, range.max), (0.0, 100.0));
        assert_eq!(range.sanitize(None), 50.0);
        assert_eq!(range.sanitize(Some("+5")), 50.0);
        assert_eq!(range.sanitize(Some("33.4")), 33.0);
        let inverted = bounds("min=20 max=10");
        assert_eq!(inverted.sanitize(None), 20.0);
        let three = bounds("min=0 max=3");
        assert_eq!(three.sanitize(None), 2.0);
        let any = bounds("step=any");
        assert_eq!(any.sanitize(Some("33.25")), 33.25);
        let bad_step = bounds("step=-2");
        assert_eq!(bad_step.sanitize(Some("2.6")), 3.0);
        // without a min the value attribute is the step base
        let based = bounds("value=0.5");
        assert_eq!(based.sanitize(Some("10")), 10.5);
    }

    #[test]
    fn decimal_steps_print_without_float_noise() {
        let range = bounds("min=0 max=1 step=0.1");
        assert_eq!(serialize(range.sanitize(Some("0.3"))), "0.3");
        assert_eq!(serialize(range.after_key(0.2, "ArrowUp").unwrap()), "0.3");
        assert_eq!(serialize(range.at_fraction(0.71)), "0.7");
        assert_eq!(serialize(bounds("").sanitize(Some("25"))), "25");
    }

    #[test]
    fn keys_step_page_and_jump_to_the_ends() {
        let range = bounds("min=0 max=200 step=5");
        assert_eq!(range.after_key(50.0, "ArrowRight"), Some(55.0));
        assert_eq!(range.after_key(50.0, "ArrowUp"), Some(55.0));
        assert_eq!(range.after_key(50.0, "ArrowLeft"), Some(45.0));
        assert_eq!(range.after_key(50.0, "PageUp"), Some(70.0));
        assert_eq!(range.after_key(50.0, "PageDown"), Some(30.0));
        assert_eq!(range.after_key(50.0, "Home"), Some(0.0));
        assert_eq!(range.after_key(50.0, "End"), Some(200.0));
        assert_eq!(range.after_key(200.0, "ArrowRight"), Some(200.0));
        assert_eq!(range.after_key(50.0, "a"), None);
        // a big step never falls below the step
        let coarse = bounds("min=0 max=10 step=4");
        assert_eq!(coarse.after_key(0.0, "PageUp"), Some(4.0));
        let any = bounds("min=0 max=50 step=any");
        assert_eq!(any.after_key(10.0, "ArrowRight"), Some(10.5));
    }
}

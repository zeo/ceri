// GLSL ES 1.00 and 3.00 for WebGL: the preprocessor and lexer, the parser, a compiler that
// checks types while it builds closures, and the machine those closures run on

mod compile;
mod exec;
mod lex;
mod parse;

pub(crate) use compile::{Base, Module, Stage, Ty, compile};
#[cfg(test)]
pub(crate) use exec::NoTextures;
pub(crate) use exec::{ALL, LANES, Lod, Machine, SamplerKind, Textures};

impl Module {
    // a machine with this module's memory, ready to run main
    pub(crate) fn machine<'a>(&self, textures: &'a dyn Textures) -> Machine<'a> {
        Machine {
            memory: self.initial.clone(),
            mask: ALL,
            broken: 0,
            continued: 0,
            returned: 0,
            discarded: 0,
            textures,
            budget: 1 << 22,
        }
    }

    // runs main on the four lanes; the lanes left discarded come back
    pub(crate) fn run(&self, machine: &mut Machine, mask: u8) -> u8 {
        machine.mask = mask;
        machine.broken = 0;
        machine.continued = 0;
        machine.returned = 0;
        machine.discarded = 0;
        machine.budget = 1 << 22;
        (self.main)(machine);
        machine.discarded
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn floats(machine: &Machine, slot: usize, count: usize, lane: usize) -> Vec<f32> {
        (0..count)
            .map(|index| machine.float(slot + index, lane))
            .collect()
    }

    // runs a fragment shader whose main writes gl_FragColor, returning lane 0's color
    fn fragment(source: &str) -> Vec<f32> {
        let module = compile(source, Stage::Fragment, &["GL_OES_standard_derivatives"])
            .unwrap_or_else(|log| panic!("{log}"));
        let textures = NoTextures;
        let mut machine = module.machine(&textures);
        module.run(&mut machine, ALL);
        floats(&machine, module.builtins["gl_FragColor"], 4, 0)
    }

    #[test]
    fn shaders_compile_and_run_on_four_lanes() {
        let vertex = compile(
            "attribute vec2 position; uniform mat2 scale; varying vec2 uv; void main() { uv = position * 0.5 + 0.5; gl_Position = vec4(scale * position, 0.0, 1.0); }",
            Stage::Vertex,
            &[],
        )
        .expect("vertex shader compiles");
        let textures = NoTextures;
        let mut machine = vertex.machine(&textures);
        let position = vertex
            .inputs
            .iter()
            .find(|input| input.name == "position")
            .expect("an attribute")
            .slot;
        let scale = vertex
            .uniforms
            .iter()
            .find(|uniform| uniform.name == "scale")
            .expect("a uniform")
            .slot;
        for lane in 0..LANES {
            machine.set_float(position, lane, lane as f32);
            machine.set_float(position + 1, lane, 1.0);
            for (index, value) in [2.0, 0.0, 0.0, 3.0].iter().enumerate() {
                machine.set_float(scale + index, lane, *value);
            }
        }
        vertex.run(&mut machine, ALL);
        assert_eq!(
            floats(&machine, vertex.builtins["gl_Position"], 4, 3),
            [6.0, 3.0, 0.0, 1.0]
        );
        let uv = vertex
            .outputs
            .iter()
            .find(|output| output.name == "uv")
            .expect("a varying")
            .slot;
        assert_eq!(floats(&machine, uv, 2, 2), [1.5, 1.0]);

        assert_eq!(
            fragment(
                "precision mediump float;
                 struct Light { vec3 color; float power; };
                 float total(float values[3]) { float sum = 0.0; for (int i = 0; i < 3; i++) { sum += values[i]; } return sum; }
                 void grow(inout vec2 v, out float done) { v *= 2.0; done = 1.0; }
                 void main() {
                     Light light = Light(vec3(0.5, 0.25, 1.0), 2.0);
                     float values[3];
                     values[0] = 1.0; values[1] = 2.0; values[2] = 3.0;
                     vec2 v = vec2(1.0, 2.0);
                     float done;
                     grow(v, done);
                     int count = 0;
                     for (int i = 0; i < 10; i++) { if (i == 7) break; if (i - (i / 2) * 2 == 1) continue; count++; }
                     mat3 m = mat3(2.0);
                     vec3 c = m * light.color * light.power;
                     gl_FragColor = vec4(c.x, total(values) + v.y, float(count) + done, dot(v, v) > 19.0 ? 1.0 : 0.0);
                 }"
            ),
            [2.0, 10.0, 5.0, 1.0]
        );
        assert_eq!(
            fragment(
                "precision highp float;
                 const int N = 2 + 1;
                 vec3 table[N];
                 void main() {
                     table[0] = vec3(1.0); table[1] = vec3(2.0); table[2] = vec3(3.0);
                     int i = 1;
                     vec3 picked = table[i + 1];
                     vec4 color = vec4(picked.zyx, 0.0).wzyx;
                     color.xy = vec2(clamp(5.0, 0.0, 1.0), mix(0.0, 10.0, 0.25));
                     color.w = length(vec2(3.0, 4.0)) + step(0.5, 0.25) + smoothstep(0.0, 1.0, 0.5);
                     gl_FragColor = color;
                 }"
            ),
            [1.0, 2.5, 3.0, 5.5]
        );
        let version3 = compile(
            "#version 300 es
             precision highp float;
             precision highp int;
             out vec4 color;
             uniform Block { vec4 tint; };
             void main() {
                 uint bits = 0xF0u >> 4u;
                 int sum = 0;
                 switch (int(bits)) { case 15: sum = 3; case 16: sum += 1; break; default: sum = -1; }
                 mat2 m = inverse(mat2(2.0, 0.0, 0.0, 4.0));
                 color = vec4(float(bits), float(sum), m[1][1], determinant(mat2(1.0, 2.0, 3.0, 4.0))) + tint;
             }",
            Stage::Fragment,
            &[],
        )
        .unwrap_or_else(|log| panic!("{log}"));
        let textures = NoTextures;
        let mut machine = version3.machine(&textures);
        version3.run(&mut machine, ALL);
        let color = version3
            .outputs
            .iter()
            .find(|output| output.name == "color")
            .expect("an output")
            .slot;
        assert_eq!(floats(&machine, color, 4, 0), [15.0, 4.0, 0.25, -2.0]);
    }

    #[test]
    fn lanes_diverge_discard_and_give_derivatives() {
        let module = compile(
            "#extension GL_OES_standard_derivatives : enable
             precision mediump float;
             varying vec2 uv;
             void main() {
                 if (uv.x > 0.5) { discard; }
                 float x = uv.x * 4.0;
                 for (float i = 0.0; i < 8.0; i += 1.0) { if (i >= uv.y) break; x += 1.0; }
                 gl_FragColor = vec4(x, dFdx(uv.x * 10.0), dFdy(uv.y), fwidth(uv.x));
             }",
            Stage::Fragment,
            &["GL_OES_standard_derivatives"],
        )
        .unwrap_or_else(|log| panic!("{log}"));
        let textures = NoTextures;
        let mut machine = module.machine(&textures);
        let uv = module.inputs[0].slot;
        // a quad: x grows to the right, y downward
        for (lane, (x, y)) in [(0.25, 1.0), (0.75, 2.0), (0.25, 3.0), (0.75, 5.0)]
            .iter()
            .enumerate()
        {
            machine.set_float(uv, lane, *x);
            machine.set_float(uv + 1, lane, *y);
        }
        let discarded = module.run(&mut machine, ALL);
        assert_eq!(discarded, 0b1010);
        let color = module.builtins["gl_FragColor"];
        assert_eq!(floats(&machine, color, 4, 0), [2.0, 5.0, 2.0, 0.5]);
        assert_eq!(floats(&machine, color, 1, 2), [4.0]);
    }

    #[test]
    fn errors_name_the_line_and_problem() {
        let fail = |source: &str| {
            compile(source, Stage::Fragment, &[])
                .err()
                .unwrap_or_default()
        };
        assert!(
            fail("void main() { float x = 1; }").contains("cannot convert from 'int' to 'float'")
        );
        assert!(fail("void main() {\n gl_FragColor = vec4(y); }").starts_with("ERROR: 0:2:"));
        assert!(fail("void main() { vec2 v; v.xz = vec2(1.0); }").contains("out of range"));
        assert!(fail("uniform float u; void main() { u = 1.0; }").contains("l-value"));
        assert!(fail("void f() {}").contains("'main'"));
        assert!(fail("#if 1\nvoid main() {}").contains("unterminated"));
        assert!(compile("#define SCALE(x) (x * 2.0)\nprecision mediump float; void main() { gl_FragColor = vec4(SCALE(0.5)); }", Stage::Fragment, &[]).is_ok());
    }

    #[test]
    fn builtins_count_as_used_only_where_named() {
        let source = |body: &str| {
            format!(
                "#version 300 es\nprecision highp float;\nout vec4 c;\n#ifdef NEVER\nvoid f() {{ gl_FragDepth = 0.5; }}\n#endif\nvoid main() {{ c = vec4(1.0); {body} }}"
            )
        };
        let skipped =
            compile(&source(""), Stage::Fragment, &[]).unwrap_or_else(|log| panic!("{log}"));
        assert!(!skipped.used_builtins.contains(&"gl_FragDepth"));
        let written = compile(&source("gl_FragDepth = 0.25;"), Stage::Fragment, &[])
            .unwrap_or_else(|log| panic!("{log}"));
        assert!(written.used_builtins.contains(&"gl_FragDepth"));
    }
}

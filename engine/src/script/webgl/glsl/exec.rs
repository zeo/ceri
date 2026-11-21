// how compiled shaders run: every value has four lanes, one per invocation of a 2x2 quad, so
// derivatives are differences between lanes; memory is a flat array of 32-bit words, slot by
// slot with the four lanes of a slot together, and control flow narrows a mask of live lanes

pub(crate) const LANES: usize = 4;
pub(crate) const ALL: u8 = 0b1111;

// what texture functions read through, which the rendering pipeline provides
pub(crate) trait Textures {
    // a texel lookup per lane: the texture unit, the sampler's dimensionality, coordinates and
    // an explicit level of detail when given, else the level from the lanes' derivatives
    fn sample(
        &self,
        unit: u32,
        kind: SamplerKind,
        coords: &[[f32; 4]; LANES],
        lod: Lod,
        out: &mut [[f32; 4]; LANES],
    );
    fn fetch(&self, unit: u32, kind: SamplerKind, coords: [i32; 4], level: i32) -> [f32; 4];
    fn size(&self, unit: u32, kind: SamplerKind, level: i32) -> [i32; 3];
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum SamplerKind {
    Two,
    Cube,
    Three,
    TwoArray,
    TwoShadow,
    CubeShadow,
    TwoArrayShadow,
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum Lod {
    // from derivatives, with a bias
    Implicit(f32),
    Explicit([f32; LANES]),
    // explicit gradients: d(coords)/dx and d(coords)/dy per lane
    Gradient([[f32; 3]; LANES], [[f32; 3]; LANES]),
}

pub(crate) struct NoTextures;

impl Textures for NoTextures {
    fn sample(
        &self,
        _: u32,
        _: SamplerKind,
        _: &[[f32; 4]; LANES],
        _: Lod,
        out: &mut [[f32; 4]; LANES],
    ) {
        *out = [[0.0, 0.0, 0.0, 1.0]; LANES];
    }

    fn fetch(&self, _: u32, _: SamplerKind, _: [i32; 4], _: i32) -> [f32; 4] {
        [0.0, 0.0, 0.0, 1.0]
    }

    fn size(&self, _: u32, _: SamplerKind, _: i32) -> [i32; 3] {
        [0, 0, 0]
    }
}

pub(crate) struct Machine<'a> {
    pub(crate) memory: Vec<u32>,
    pub(crate) mask: u8,
    pub(crate) broken: u8,
    pub(crate) continued: u8,
    pub(crate) returned: u8,
    pub(crate) discarded: u8,
    pub(crate) textures: &'a dyn Textures,
    // loops left to run before a runaway shader is stopped
    pub(crate) budget: u64,
}

pub(crate) type Op = Box<dyn Fn(&mut Machine)>;

impl Machine<'_> {
    #[inline]
    pub(crate) fn get(&self, slot: usize, lane: usize) -> u32 {
        self.memory[slot * LANES + lane]
    }

    #[inline]
    pub(crate) fn set(&mut self, slot: usize, lane: usize, value: u32) {
        self.memory[slot * LANES + lane] = value;
    }

    #[inline]
    pub(crate) fn float(&self, slot: usize, lane: usize) -> f32 {
        f32::from_bits(self.get(slot, lane))
    }

    #[inline]
    pub(crate) fn set_float(&mut self, slot: usize, lane: usize, value: f32) {
        self.set(slot, lane, value.to_bits());
    }

    // the lanes not stopped by break, continue, return or discard
    #[inline]
    pub(crate) fn alive(&self) -> u8 {
        !(self.broken | self.continued | self.returned | self.discarded) & ALL
    }

    // copies slots for the lanes in the mask
    pub(crate) fn copy_masked(&mut self, from: usize, to: usize, count: usize, mask: u8) {
        for slot in 0..count {
            for lane in 0..LANES {
                if mask & (1 << lane) != 0 {
                    let value = self.get(from + slot, lane);
                    self.set(to + slot, lane, value);
                }
            }
        }
    }

    pub(crate) fn copy_all(&mut self, from: usize, to: usize, count: usize) {
        let (from, to) = (from * LANES, to * LANES);
        self.memory.copy_within(from..from + count * LANES, to);
    }
}

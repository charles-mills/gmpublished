//! Named numeric narrowing (saturating/checked) so protocol code never
//! scatters bare `as` casts. Graphics/physics inner loops are exempt: `as`
//! on indices and texel math is the domain idiom there.

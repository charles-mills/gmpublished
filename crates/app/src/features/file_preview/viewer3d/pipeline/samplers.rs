use super::wgpu;
use parking_lot::Mutex;
use std::collections::HashMap;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct SamplerQuality {
    pub anisotropy: u16,
    pub force_trilinear: bool,
}

const APPLICATION_QUALITY: SamplerQuality = SamplerQuality {
    anisotropy: 16,
    force_trilinear: true,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum TextureKind {
    Texture2d,
    Cubemap,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct TextureSamplerPolicy {
    pub address: [wgpu::AddressMode; 3],
    pub min_filter: wgpu::FilterMode,
    pub mag_filter: wgpu::FilterMode,
    pub mip_filter: wgpu::FilterMode,
}

impl TextureSamplerPolicy {
    pub fn from_flags(texture_flags: u32, kind: TextureKind) -> Self {
        use vformats::vtf::texture_flags as flags;
        let address = |flag| {
            if kind == TextureKind::Cubemap || texture_flags & flag != 0 {
                wgpu::AddressMode::ClampToEdge
            } else {
                wgpu::AddressMode::Repeat
            }
        };
        let filter = if texture_flags & flags::POINTSAMPLE != 0 {
            wgpu::FilterMode::Nearest
        } else {
            wgpu::FilterMode::Linear
        };
        Self {
            address: [
                address(flags::CLAMPS),
                address(flags::CLAMPT),
                wgpu::AddressMode::ClampToEdge,
            ],
            min_filter: filter,
            mag_filter: filter,
            mip_filter: if texture_flags & (flags::TRILINEAR | flags::ANISOTROPIC) != 0 {
                wgpu::FilterMode::Linear
            } else {
                wgpu::FilterMode::Nearest
            },
        }
    }

    pub fn request(self, quality: SamplerQuality) -> Result<RequestedSamplerPolicy, PolicyError> {
        if quality.anisotropy == 0 {
            return Err(PolicyError::ZeroAnisotropy);
        }
        Ok(RequestedSamplerPolicy {
            descriptor: SamplerKey {
                address: self.address,
                min_filter: self.min_filter,
                mag_filter: self.mag_filter,
                mip_filter: if quality.force_trilinear
                    && self.min_filter == wgpu::FilterMode::Linear
                    && self.mag_filter == wgpu::FilterMode::Linear
                {
                    wgpu::FilterMode::Linear
                } else {
                    self.mip_filter
                },
                anisotropy: quality.anisotropy,
            },
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub(super) enum PolicyError {
    #[error("sampler anisotropy must be at least one")]
    ZeroAnisotropy,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(super) struct SamplerKey {
    pub address: [wgpu::AddressMode; 3],
    pub min_filter: wgpu::FilterMode,
    pub mag_filter: wgpu::FilterMode,
    pub mip_filter: wgpu::FilterMode,
    pub anisotropy: u16,
}

impl SamplerKey {
    fn descriptor(self) -> wgpu::SamplerDescriptor<'static> {
        wgpu::SamplerDescriptor {
            label: Some("file_preview.texture_sampler"),
            address_mode_u: self.address[0],
            address_mode_v: self.address[1],
            address_mode_w: self.address[2],
            mag_filter: self.mag_filter,
            min_filter: self.min_filter,
            mipmap_filter: self.mip_filter,
            anisotropy_clamp: self.anisotropy,
            ..Default::default()
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct RequestedSamplerPolicy {
    descriptor: SamplerKey,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum SamplerLimitation {
    PortableAnisotropyLimit { requested: u16, maximum: u16 },
    AnisotropyUnavailable,
    NativeAnisotropicPointUnavailable,
    IndependentAnisotropicMipUnavailable,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct SamplerCapabilities {
    pub anisotropy: bool,
    pub independent_anisotropic_mip: bool,
    pub native_anisotropic_point: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct AppliedSamplerPolicy {
    #[cfg(test)]
    pub requested: RequestedSamplerPolicy,
    pub descriptor: SamplerKey,
    #[cfg(test)]
    pub limitations: Vec<SamplerLimitation>,
}

impl RequestedSamplerPolicy {
    pub fn descriptor(self) -> SamplerKey {
        self.descriptor
    }

    pub fn apply(self, capabilities: SamplerCapabilities) -> AppliedSamplerPolicy {
        let mut descriptor = self.descriptor();
        #[cfg(test)]
        let mut limitations = Vec::new();
        if descriptor.anisotropy > 16 {
            #[cfg(test)]
            limitations.push(SamplerLimitation::PortableAnisotropyLimit {
                requested: descriptor.anisotropy,
                maximum: 16,
            });
            descriptor.anisotropy = 16;
        }
        if descriptor.anisotropy > 1 {
            let min_mag_linear = descriptor.min_filter == wgpu::FilterMode::Linear
                && descriptor.mag_filter == wgpu::FilterMode::Linear;
            let min_mag_supported = min_mag_linear || capabilities.native_anisotropic_point;
            let mip_supported = descriptor.mip_filter == wgpu::FilterMode::Linear
                || capabilities.independent_anisotropic_mip;
            #[cfg(test)]
            if !capabilities.anisotropy {
                limitations.push(SamplerLimitation::AnisotropyUnavailable);
            }
            #[cfg(test)]
            if !min_mag_supported {
                limitations.push(SamplerLimitation::NativeAnisotropicPointUnavailable);
            }
            #[cfg(test)]
            if !mip_supported {
                limitations.push(SamplerLimitation::IndependentAnisotropicMipUnavailable);
            }
            if !capabilities.anisotropy || !min_mag_supported || !mip_supported {
                descriptor.anisotropy = 1;
            }
        }
        AppliedSamplerPolicy {
            #[cfg(test)]
            requested: self,
            descriptor,
            #[cfg(test)]
            limitations,
        }
    }
}

#[derive(Debug)]
pub(super) struct SamplerCache {
    device: wgpu::Device,
    capabilities: SamplerCapabilities,
    samplers: Mutex<HashMap<SamplerKey, wgpu::Sampler>>,
}

#[derive(Clone, Debug)]
pub(super) struct ResolvedSampler {
    pub sampler: wgpu::Sampler,
    #[cfg(test)]
    pub policy: AppliedSamplerPolicy,
}

impl SamplerCache {
    pub fn new(device: &wgpu::Device, anisotropy_supported: bool) -> Self {
        Self {
            device: device.clone(),
            capabilities: SamplerCapabilities {
                anisotropy: anisotropy_supported,
                independent_anisotropic_mip: device
                    .features()
                    .contains(wgpu::Features::INDEPENDENT_ANISOTROPIC_MIP_FILTER),
                native_anisotropic_point: device
                    .features()
                    .contains(wgpu::Features::NATIVE_ANISOTROPIC_POINT_FILTERING),
            },
            samplers: Mutex::new(HashMap::new()),
        }
    }

    pub fn resolve(&self, requested: RequestedSamplerPolicy) -> ResolvedSampler {
        let policy = requested.apply(self.capabilities);
        let sampler = intern(&self.samplers, policy.descriptor, || {
            self.device.create_sampler(&policy.descriptor.descriptor())
        });
        ResolvedSampler {
            sampler,
            #[cfg(test)]
            policy,
        }
    }

    pub fn texture(&self, flags: Option<u32>, kind: TextureKind) -> wgpu::Sampler {
        let policy = TextureSamplerPolicy::from_flags(
            flags.unwrap_or(vformats::vtf::texture_flags::TRILINEAR),
            kind,
        );
        let resolved = self.resolve(
            policy
                .request(APPLICATION_QUALITY)
                .expect("validated sampler quality"),
        );
        resolved.sampler
    }
}

fn intern<T: Clone>(
    cache: &Mutex<HashMap<SamplerKey, T>>,
    key: SamplerKey,
    create: impl FnOnce() -> T,
) -> T {
    cache.lock().entry(key).or_insert_with(create).clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use vformats::vtf::texture_flags as flags;

    fn portable(anisotropy: bool) -> SamplerCapabilities {
        SamplerCapabilities {
            anisotropy,
            independent_anisotropic_mip: false,
            native_anisotropic_point: false,
        }
    }

    fn request(flags: u32, anisotropy: u16, force_trilinear: bool) -> RequestedSamplerPolicy {
        TextureSamplerPolicy::from_flags(flags, TextureKind::Texture2d)
            .request(SamplerQuality {
                anisotropy,
                force_trilinear,
            })
            .unwrap()
    }

    #[test]
    fn point_anisotropy_and_nearest_mip_require_independent_capabilities() {
        use wgpu::FilterMode::{Linear, Nearest};
        for native_point in [false, true] {
            for native_mip in [false, true] {
                for min in [Nearest, Linear] {
                    for mag in [Nearest, Linear] {
                        for mip in [Nearest, Linear] {
                            let requested = TextureSamplerPolicy {
                                address: [wgpu::AddressMode::Repeat; 3],
                                min_filter: min,
                                mag_filter: mag,
                                mip_filter: mip,
                            }
                            .request(SamplerQuality {
                                anisotropy: 4,
                                force_trilinear: false,
                            })
                            .unwrap();
                            let applied = requested.apply(SamplerCapabilities {
                                anisotropy: true,
                                independent_anisotropic_mip: native_mip,
                                native_anisotropic_point: native_point,
                            });
                            let point_supported = (min == Linear && mag == Linear) || native_point;
                            let mip_supported = mip == Linear || native_mip;
                            assert_eq!(
                                applied.descriptor.anisotropy,
                                if point_supported && mip_supported {
                                    4
                                } else {
                                    1
                                }
                            );
                            assert_eq!(applied.descriptor.min_filter, min);
                            assert_eq!(applied.descriptor.mag_filter, mag);
                            assert_eq!(applied.descriptor.mip_filter, mip);
                            assert_eq!(
                                applied.limitations.contains(
                                    &SamplerLimitation::NativeAnisotropicPointUnavailable
                                ),
                                !point_supported
                            );
                            assert_eq!(
                                applied.limitations.contains(
                                    &SamplerLimitation::IndependentAnisotropicMipUnavailable
                                ),
                                !mip_supported
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn texture_filter_flags_and_axes_are_independent() {
        for bits in 0..32 {
            let p = TextureSamplerPolicy::from_flags(bits, TextureKind::Texture2d);
            let point = bits & flags::POINTSAMPLE != 0;
            assert_eq!(p.min_filter == wgpu::FilterMode::Nearest, point);
            assert_eq!(p.mag_filter, p.min_filter);
            assert_eq!(
                p.mip_filter == wgpu::FilterMode::Linear,
                bits & (flags::TRILINEAR | flags::ANISOTROPIC) != 0
            );
            assert_eq!(
                p.address[0] == wgpu::AddressMode::ClampToEdge,
                bits & flags::CLAMPS != 0
            );
            assert_eq!(
                p.address[1] == wgpu::AddressMode::ClampToEdge,
                bits & flags::CLAMPT != 0
            );
            assert_eq!(p.address[2], wgpu::AddressMode::ClampToEdge);
        }
    }

    #[test]
    fn cubes_clamp_axes_and_preserve_authored_mip_filtering() {
        let p = TextureSamplerPolicy::from_flags(
            flags::POINTSAMPLE | flags::ANISOTROPIC,
            TextureKind::Cubemap,
        );
        assert_eq!(p.address, [wgpu::AddressMode::ClampToEdge; 3]);
        assert_eq!(p.min_filter, wgpu::FilterMode::Nearest);
        assert_eq!(p.mip_filter, wgpu::FilterMode::Linear);
        let plain = TextureSamplerPolicy::from_flags(0, TextureKind::Cubemap)
            .request(SamplerQuality {
                anisotropy: 4,
                force_trilinear: false,
            })
            .unwrap()
            .apply(SamplerCapabilities {
                anisotropy: true,
                independent_anisotropic_mip: true,
                native_anisotropic_point: false,
            });
        assert_eq!(plain.descriptor.mip_filter, wgpu::FilterMode::Nearest);
        assert_eq!(plain.descriptor.anisotropy, 4);
        assert!(plain.limitations.is_empty());
    }

    #[test]
    fn explicit_quality_overrides_mips_without_erasing_point_sampling() {
        let p = request(flags::POINTSAMPLE, 16, true).apply(portable(true));
        assert_eq!(p.requested.descriptor.anisotropy, 16);
        assert_eq!(p.descriptor.anisotropy, 1);
        assert_eq!(p.descriptor.mip_filter, wgpu::FilterMode::Nearest);
        assert_eq!(p.descriptor.mag_filter, wgpu::FilterMode::Nearest);
        assert_eq!(
            p.limitations,
            [
                SamplerLimitation::NativeAnisotropicPointUnavailable,
                SamplerLimitation::IndependentAnisotropicMipUnavailable
            ]
        );
        assert!(
            request(0, 16, true)
                .apply(portable(true))
                .limitations
                .is_empty()
        );
    }

    #[test]
    fn nearest_mips_are_preserved_when_anisotropy_cannot_be_expressed() {
        let p = request(0, 4, false).apply(portable(true));
        assert_eq!(p.descriptor.anisotropy, 1);
        assert_eq!(p.descriptor.mip_filter, wgpu::FilterMode::Nearest);
        assert_eq!(
            p.limitations,
            [SamplerLimitation::IndependentAnisotropicMipUnavailable]
        );
    }

    #[test]
    fn independent_mip_capability_preserves_native_request_without_relaxing_point_filters() {
        let capabilities = SamplerCapabilities {
            anisotropy: true,
            independent_anisotropic_mip: true,
            native_anisotropic_point: false,
        };
        let policy = request(0, 4, false).apply(capabilities);
        assert_eq!(policy.descriptor, policy.requested.descriptor());
        assert!(policy.limitations.is_empty());
        let point = request(flags::POINTSAMPLE, 4, false).apply(capabilities);
        assert_eq!(point.descriptor.anisotropy, 1);
        assert_eq!(point.descriptor.mip_filter, wgpu::FilterMode::Nearest);
        assert_eq!(
            point.limitations,
            [SamplerLimitation::NativeAnisotropicPointUnavailable]
        );
    }

    #[test]
    fn capability_and_portable_limit_are_reported_separately() {
        let p = request(flags::TRILINEAR, 32, false).apply(portable(false));
        assert_eq!(p.descriptor.anisotropy, 1);
        assert_eq!(
            p.limitations,
            [
                SamplerLimitation::PortableAnisotropyLimit {
                    requested: 32,
                    maximum: 16
                },
                SamplerLimitation::AnisotropyUnavailable
            ]
        );
        assert_eq!(
            request(flags::TRILINEAR, 32, false)
                .apply(portable(true))
                .descriptor
                .anisotropy,
            16
        );
        assert_eq!(
            TextureSamplerPolicy::from_flags(0, TextureKind::Texture2d).request(SamplerQuality {
                anisotropy: 0,
                force_trilinear: false
            }),
            Err(PolicyError::ZeroAnisotropy)
        );
    }

    #[test]
    fn all_supported_requests_lower_to_legal_wgpu_descriptors() {
        for flags in 0..32 {
            for anisotropy in [1, 2, 4, 16, 17, u16::MAX] {
                for force in [false, true] {
                    for supported in [false, true] {
                        let p = request(flags, anisotropy, force).apply(portable(supported));
                        let d = p.descriptor.descriptor();
                        assert!((1..=16).contains(&d.anisotropy_clamp));
                        if d.anisotropy_clamp > 1 {
                            assert!(supported);
                            assert_eq!(
                                [d.min_filter, d.mag_filter, d.mipmap_filter],
                                [wgpu::FilterMode::Linear; 3]
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn lowered_requests_share_objects_without_losing_requested_quality() {
        let cache = Mutex::new(HashMap::new());
        let a = request(0, 4, false).apply(portable(true));
        let b = request(0, 1, false).apply(portable(true));
        assert_eq!(a.descriptor, b.descriptor);
        assert_ne!(a.requested, b.requested);
        assert!(!a.limitations.is_empty());
        assert!(b.limitations.is_empty());
        let first = intern(&cache, a.descriptor, || Arc::new(7));
        let second = intern(&cache, b.descriptor, || panic!("duplicate allocation"));
        assert!(Arc::ptr_eq(&first, &second));
        let clamped = request(flags::CLAMPS, 1, false).apply(portable(true));
        let other = intern(&cache, clamped.descriptor, || Arc::new(7));
        assert!(!Arc::ptr_eq(&first, &other));
    }

    #[test]
    fn simultaneous_requests_create_one_object_per_applied_descriptor() {
        let cache = Mutex::new(HashMap::new());
        let count = AtomicUsize::new(0);
        let key = request(flags::TRILINEAR, 4, false)
            .apply(portable(true))
            .descriptor;
        std::thread::scope(|scope| {
            for _ in 0..16 {
                scope.spawn(|| {
                    assert_eq!(
                        intern(&cache, key, || count.fetch_add(1, Ordering::SeqCst)),
                        0
                    );
                });
            }
        });
        assert_eq!(count.load(Ordering::SeqCst), 1);
        assert_eq!(cache.lock().len(), 1);
    }
}

#[cfg(test)]
#[path = "samplers/gpu_tests.rs"]
mod gpu_tests;

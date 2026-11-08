use crate::internal::*;
use tract_core::ops;

#[cfg(not(feature = "minimal_core_registry"))]
mod broadcast;
mod cast;
#[cfg(not(feature = "minimal_core_registry"))]
mod clamped_swiglu;
#[cfg(feature = "complex")]
mod complex;
#[cfg(not(feature = "minimal_core_registry"))]
mod cumsum;
#[cfg(not(feature = "minimal_core_registry"))]
mod downsample;
#[cfg(not(feature = "minimal_core_registry"))]
mod dyn_slice;
#[cfg(not(feature = "minimal_core_registry"))]
mod einsum;
#[cfg(not(feature = "minimal_core_registry"))]
mod fft;
#[cfg(not(feature = "minimal_core_registry"))]
mod gather;
#[cfg(not(feature = "minimal_core_registry"))]
mod gelu_approximate;
#[cfg(not(feature = "minimal_core_registry"))]
mod grid_sample;
#[cfg(not(feature = "minimal_core_registry"))]
mod gru_cell;
#[cfg(not(feature = "minimal_core_registry"))]
mod gru_seq;
#[cfg(not(feature = "minimal_core_registry"))]
mod is_inf;
#[cfg(not(feature = "minimal_core_registry"))]
mod lstm_cell;
#[cfg(not(feature = "minimal_core_registry"))]
mod matmul;
#[cfg(not(feature = "minimal_core_registry"))]
mod one_hot;
#[cfg(not(feature = "minimal_core_registry"))]
mod pow_const;
#[cfg(not(feature = "minimal_core_registry"))]
mod qconv;
#[cfg(not(feature = "minimal_core_registry"))]
mod qmatmul;
#[cfg(not(feature = "minimal_core_registry"))]
mod range;
#[cfg(not(feature = "minimal_core_registry"))]
mod reduce;
#[cfg(not(feature = "minimal_core_registry"))]
mod resize;
#[cfg(not(feature = "minimal_core_registry"))]
mod rms_norm;
#[cfg(not(feature = "minimal_core_registry"))]
mod scan;
#[cfg(not(feature = "minimal_core_registry"))]
mod scatter;
#[cfg(not(feature = "minimal_core_registry"))]
mod shape_of;
#[cfg(not(feature = "minimal_core_registry"))]
mod silu;
#[cfg(not(feature = "minimal_core_registry"))]
mod softmax;
#[cfg(not(feature = "minimal_core_registry"))]
mod source;
#[cfg(not(feature = "minimal_core_registry"))]
mod submodel;
#[cfg(not(feature = "minimal_core_registry"))]
mod topk;
#[cfg(not(feature = "minimal_core_registry"))]
mod trilu;

pub fn register(registry: &mut Registry) {
    registry.register_unit_element_wise("tract_core_round_even", &ops::math::RoundHalfToEven {});
    registry.register_unit_element_wise("tract_core_erf", &ops::math::Erf {});
    registry.register_unit_element_wise("tract_core_hard_swish", &ops::nn::HardSwish {});

    registry.register_binary("tract_core_xor", &ops::logic::Xor {});
    registry.register_binary("tract_core_bitand", &ops::logic::BitAnd {});
    registry.register_binary("tract_core_bitor", &ops::logic::BitOr {});
    registry.register_binary("tract_core_bitxor", &ops::logic::BitXor {});
    registry.register_unit_element_wise("tract_core_bitnot", &ops::logic::BitNot {});

    registry.register_unit_element_wise("tract_core_is_nan", &ops::math::IsNan {});

    registry.register_binary("tract_shl", &ops::math::ShiftLeft);
    registry.register_binary("tract_shr", &ops::math::ShiftRight);
    cast::register(registry);
    #[cfg(not(feature = "minimal_core_registry"))]
    register_operators(registry);
}

/// The core registry's operators beyond the element-wise ones and the cast (see the
/// `minimal_core_registry` feature).
#[cfg(not(feature = "minimal_core_registry"))]
fn register_operators(registry: &mut Registry) {
    broadcast::register(registry);
    clamped_swiglu::register(registry);
    #[cfg(feature = "complex")]
    complex::register(registry);
    cumsum::register(registry);
    downsample::register(registry);
    dyn_slice::register(registry);
    einsum::register(registry);
    fft::register(registry);
    gather::register(registry);
    gelu_approximate::register(registry);
    pow_const::register(registry);
    grid_sample::register(registry);
    gru_cell::register(registry);
    gru_seq::register(registry);
    lstm_cell::register(registry);
    matmul::register(registry);
    one_hot::register(registry);
    qconv::register(registry);
    qmatmul::register(registry);
    reduce::register(registry);
    resize::register(registry);
    rms_norm::register(registry);
    scan::register(registry);
    scatter::register(registry);
    shape_of::register(registry);
    silu::register(registry);
    softmax::register(registry);
    source::register(registry);
    submodel::register(registry);
    range::register(registry);
    topk::register(registry);
    trilu::register(registry);
    is_inf::register(registry);
}

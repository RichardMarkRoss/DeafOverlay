//! WAVEFORMATEXTENSIBLE channel mask -> speaker azimuth per captured channel
//! (port of v1 `ma_channel_to_azimuth` + `default_azimuth`).

/// Speaker bits in channel order, with azimuth in degrees (0 front, + right). `None` = no
/// meaningful direction (LFE). Height speakers (bits 11+) also map to `None`.
const SPEAKERS: [(u32, Option<f32>); 11] = [
    (0x1, Some(-30.0)),   // FRONT_LEFT
    (0x2, Some(30.0)),    // FRONT_RIGHT
    (0x4, Some(0.0)),     // FRONT_CENTER
    (0x8, None),          // LOW_FREQUENCY
    (0x10, Some(-135.0)), // BACK_LEFT
    (0x20, Some(135.0)),  // BACK_RIGHT
    (0x40, Some(-15.0)),  // FRONT_LEFT_OF_CENTER
    (0x80, Some(15.0)),   // FRONT_RIGHT_OF_CENTER
    (0x100, Some(180.0)), // BACK_CENTER
    (0x200, Some(-90.0)), // SIDE_LEFT
    (0x400, Some(90.0)),  // SIDE_RIGHT
];

/// Azimuth per channel, in the order WASAPI interleaves them (ascending mask bit).
pub fn channel_azimuths(mask: u32, channels: usize) -> Vec<Option<f32>> {
    let mut az: Vec<Option<f32>> = (0..32)
        .map(|b| 1u32 << b)
        .filter(|bit| mask & bit != 0)
        .map(|bit| SPEAKERS.iter().find(|(m, _)| *m == bit).and_then(|(_, a)| *a))
        .take(channels)
        .collect();
    if az.len() < channels {
        return default_azimuths(channels);
    }
    // 5.1 has one surround pair, whichever bits the driver uses (BL/BR or SL/SR). Content is
    // mixed for the ITU position of ±110°, not the 7.1 back (±135°) or side (±90°) spots.
    if channels <= 6 {
        for a in az.iter_mut().flatten() {
            if a.abs() >= 90.0 && a.abs() < 180.0 {
                *a = 110f32.copysign(*a);
            }
        }
    }
    az
}

/// No usable mask: standard WASAPI order by channel count, like v1 `default_azimuth`.
fn default_azimuths(channels: usize) -> Vec<Option<f32>> {
    let v: &[Option<f32>] = match channels {
        2 => &[Some(-30.0), Some(30.0)],
        6 => &[Some(-30.0), Some(30.0), Some(0.0), None, Some(-110.0), Some(110.0)],
        8 => &[Some(-30.0), Some(30.0), Some(0.0), None, Some(-135.0), Some(135.0), Some(-90.0), Some(90.0)],
        n => return vec![Some(0.0); n], // mono / unknown collapses to front-centre
    };
    v.to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standard_masks() {
        assert_eq!(channel_azimuths(0x3, 2), [Some(-30.0), Some(30.0)]);
        // 7.1 (KSAUDIO_SPEAKER_7POINT1_SURROUND): FL FR FC LFE BL BR SL SR
        assert_eq!(
            channel_azimuths(0x63F, 8),
            [Some(-30.0), Some(30.0), Some(0.0), None, Some(-135.0), Some(135.0), Some(-90.0), Some(90.0)]
        );
        // 5.1 with back bits and with side bits both land at ±110
        let back = channel_azimuths(0x3F, 6);
        let side = channel_azimuths(0x60F, 6);
        assert_eq!(back, side);
        assert_eq!(back[4..], [Some(-110.0), Some(110.0)]);
        // no mask -> default by count
        assert_eq!(channel_azimuths(0, 6)[3], None);
    }
}

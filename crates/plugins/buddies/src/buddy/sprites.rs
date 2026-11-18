// 由 scripts/buddies_pack.py 生成，别手改：改 buddies_render.py 重渲再打包。

use super::{Dangle, Sprite, Wearable};

pub(super) const BLOB: &[u8] = include_bytes!("../../assets/buddies.bin");

/// 会转的零件每组几个关键朝向（每 45° 一张，从 0° 起）。
pub(super) const KEYS: usize = 8;
/// 跟着身子转身的零件每组几个朝向（每 22° 一张，0 是正面，往画面右边转为正）。
pub(super) const FACINGS: usize = 16;
/// 身子色乘到中性灰的身子、胳膊上的系数（线性 RGB，乘完超过 1 的截断）。顺序：rose、salmon、apricot、lemon、lime、green、mint、aqua、sky、periwinkle、lilac、orchid。
pub(super) const BODY_TINTS: [[f32; 3]; 12] = [
    [8.3377, 4.3517, 5.3374],
    [8.5713, 4.4685, 3.9023],
    [7.9592, 4.8370, 2.9768],
    [6.4107, 5.4750, 2.6976],
    [4.8101, 6.0299, 3.1781],
    [3.4346, 6.4123, 4.2854],
    [2.7229, 6.5096, 5.5700],
    [2.5352, 6.3856, 6.9617],
    [3.0218, 5.9945, 8.3494],
    [4.3824, 5.3705, 8.8792],
    [5.9002, 4.8611, 8.3552],
    [7.3481, 4.4956, 6.9828],
];
/// 戴的东西的颜色乘到中性灰的主料上的系数（线性 RGB，乘完超过 1 的截断）。顺序：red、amber、green、teal、blue、plum、charcoal。
pub(super) const WEAR_TINTS: [[f32; 3]; 7] = [
    [5.0946, 0.6204, 0.5642],
    [6.4676, 2.9672, 0.2699],
    [0.3294, 2.1733, 0.6587],
    [0.0000, 2.3145, 2.5362],
    [0.3034, 1.0620, 3.6714],
    [1.9410, 0.6059, 2.3721],
    [0.4242, 0.4933, 0.6174],
];
/// 每种身子色配哪些戴的颜色（第 i 位是 `WEAR_TINTS` 第 i 种）：色相太近或色差太小的不配，
/// 戴上糊成一片（见 `buddies_pack.py` 的 `clashes`）。
pub(super) const WEAR_FOR_BODY: [u16; 12] = [
    0b1111100, 0b1111100, 0b1111100, 0b1111101, 0b1111001, 0b1111001, 0b1110011, 0b1110111,
    0b1100111, 0b1101111, 0b1011111, 0b1011111,
];

/// 戴的东西，头顶、脸上、脖子各一张表：可换色的主料、固定色的点缀各是哪张，头顶会晃的部分和它每个朝向的挂点。
pub(super) static HEADS: [Wearable; 15] = [
    Wearable {
        base: Some(HEAD_BEANIE_BASE),
        trim: None,
        eyes: false,
        dangle: Some(Dangle {
            base: None,
            trim: Some(HEAD_BEANIE_DANGLE_TRIM),
            pivots: [
                (0.0, -22.36),
                (0.0, -22.36),
                (0.0, -22.36),
                (0.0, -22.36),
                (0.0, -22.36),
                (0.0, -22.36),
                (0.0, -22.36),
                (0.0, -22.36),
                (0.0, -22.36),
                (0.0, -22.36),
                (0.0, -22.36),
                (0.0, -22.36),
                (0.0, -22.36),
                (0.0, -22.36),
                (0.0, -22.36),
                (0.0, -22.36),
            ],
        }),
    }, // beanie
    Wearable {
        base: Some(HEAD_CAP_BASE),
        trim: None,
        eyes: false,
        dangle: None,
    }, // cap
    Wearable {
        base: Some(HEAD_FEDORA_BASE),
        trim: Some(HEAD_FEDORA_TRIM),
        eyes: false,
        dangle: None,
    }, // fedora
    Wearable {
        base: Some(HEAD_BERET_BASE),
        trim: None,
        eyes: false,
        dangle: None,
    }, // beret
    Wearable {
        base: Some(HEAD_GOGGLES_BASE),
        trim: Some(HEAD_GOGGLES_TRIM),
        eyes: true,
        dangle: None,
    }, // goggles
    Wearable {
        base: Some(HEAD_STRAW_BASE),
        trim: Some(HEAD_STRAW_TRIM),
        eyes: false,
        dangle: None,
    }, // straw
    Wearable {
        base: Some(HEAD_BANDANA_BASE),
        trim: Some(HEAD_BANDANA_TRIM),
        eyes: false,
        dangle: None,
    }, // bandana
    Wearable {
        base: Some(HEAD_HELMET_BASE),
        trim: Some(HEAD_HELMET_TRIM),
        eyes: false,
        dangle: None,
    }, // helmet
    Wearable {
        base: None,
        trim: Some(HEAD_SLICK_TRIM),
        eyes: false,
        dangle: None,
    }, // slick
    Wearable {
        base: None,
        trim: None,
        eyes: false,
        dangle: Some(Dangle {
            base: None,
            trim: Some(HEAD_SPRIG_DANGLE_TRIM),
            pivots: [
                (0.0, -21.665),
                (0.0, -21.665),
                (0.0, -21.665),
                (0.0, -21.665),
                (0.0, -21.665),
                (0.0, -21.665),
                (0.0, -21.665),
                (0.0, -21.665),
                (0.0, -21.665),
                (0.0, -21.665),
                (0.0, -21.665),
                (0.0, -21.665),
                (0.0, -21.665),
                (0.0, -21.665),
                (0.0, -21.665),
                (0.0, -21.665),
            ],
        }),
    }, // sprig
    Wearable {
        base: Some(HEAD_FLOWER_BASE),
        trim: Some(HEAD_FLOWER_TRIM),
        eyes: false,
        dangle: None,
    }, // flower
    Wearable {
        base: Some(HEAD_HEADPHONES_BASE),
        trim: Some(HEAD_HEADPHONES_TRIM),
        eyes: false,
        dangle: None,
    }, // headphones
    Wearable {
        base: None,
        trim: None,
        eyes: false,
        dangle: Some(Dangle {
            base: Some(HEAD_ANTENNA_DANGLE_BASE),
            trim: Some(HEAD_ANTENNA_DANGLE_TRIM),
            pivots: [
                (0.58, -21.765),
                (0.405, -21.82),
                (0.165, -21.85),
                (-0.1, -21.855),
                (-0.35, -21.83),
                (-0.545, -21.78),
                (-0.66, -21.715),
                (-0.67, -21.64),
                (-0.58, -21.57),
                (-0.405, -21.515),
                (-0.165, -21.485),
                (0.1, -21.48),
                (0.35, -21.505),
                (0.545, -21.555),
                (0.66, -21.62),
                (0.67, -21.695),
            ],
        }),
    }, // antenna
    Wearable {
        base: None,
        trim: Some(HEAD_CHEF_TRIM),
        eyes: false,
        dangle: None,
    }, // chef
    Wearable {
        base: Some(HEAD_BOW_BASE),
        trim: None,
        eyes: false,
        dangle: None,
    }, // bow
];
pub(super) static FACES: [Wearable; 2] = [
    Wearable {
        base: Some(FACE_GLASSES_BASE),
        trim: None,
        eyes: true,
        dangle: None,
    }, // glasses
    Wearable {
        base: Some(FACE_SUNGLASSES_BASE),
        trim: Some(FACE_SUNGLASSES_TRIM),
        eyes: true,
        dangle: None,
    }, // sunglasses
];
pub(super) static NECKS: [Wearable; 2] = [
    Wearable {
        base: Some(NECK_BOW_TIE_BASE),
        trim: None,
        eyes: false,
        dangle: None,
    }, // bow-tie
    Wearable {
        base: Some(NECK_SCARF_BASE),
        trim: None,
        eyes: false,
        dangle: None,
    }, // scarf
];

/// 每组第一张的编号。
pub(super) const BODY: usize = 0;
pub(super) const BLUSH: usize = 16;
pub(super) const ARM: usize = 32;
pub(super) const PROP_WRENCH: usize = 40;
pub(super) const PROP_MAGNIFIER: usize = 48;
pub(super) const PROP_PENCIL: usize = 56;
pub(super) const PROP_FLASK: usize = 64;
pub(super) const PROP_CAN: usize = 72;
pub(super) const PROP_BATON: usize = 80;
pub(super) const PROP_BROOM: usize = 88;
pub(super) const EYE_OPEN_L: usize = 96;
pub(super) const EYE_OPEN_R: usize = 112;
pub(super) const EYE_HAPPY_L: usize = 128;
pub(super) const EYE_HAPPY_R: usize = 144;
pub(super) const EYE_SHUT_L: usize = 160;
pub(super) const EYE_SHUT_R: usize = 176;
pub(super) const EYE_SLEEPY_L: usize = 192;
pub(super) const EYE_SLEEPY_R: usize = 208;
pub(super) const EYE_WIDE_L: usize = 224;
pub(super) const EYE_WIDE_R: usize = 240;
pub(super) const MOUTH_SMILE: usize = 256;
pub(super) const MOUTH_FLAT: usize = 272;
pub(super) const MOUTH_OPEN: usize = 288;
pub(super) const MOUTH_O: usize = 304;
pub(super) const MOUTH_YAWN: usize = 320;
pub(super) const HEAD_BEANIE_BASE: usize = 336;
pub(super) const HEAD_CAP_BASE: usize = 352;
pub(super) const HEAD_FEDORA_BASE: usize = 368;
pub(super) const HEAD_FEDORA_TRIM: usize = 384;
pub(super) const HEAD_BERET_BASE: usize = 400;
pub(super) const HEAD_GOGGLES_BASE: usize = 416;
pub(super) const HEAD_GOGGLES_TRIM: usize = 432;
pub(super) const HEAD_STRAW_BASE: usize = 448;
pub(super) const HEAD_STRAW_TRIM: usize = 464;
pub(super) const HEAD_BANDANA_BASE: usize = 480;
pub(super) const HEAD_BANDANA_TRIM: usize = 496;
pub(super) const HEAD_HELMET_BASE: usize = 512;
pub(super) const HEAD_HELMET_TRIM: usize = 528;
pub(super) const HEAD_SLICK_TRIM: usize = 544;
pub(super) const HEAD_FLOWER_BASE: usize = 560;
pub(super) const HEAD_FLOWER_TRIM: usize = 576;
pub(super) const HEAD_HEADPHONES_BASE: usize = 592;
pub(super) const HEAD_HEADPHONES_TRIM: usize = 608;
pub(super) const HEAD_CHEF_TRIM: usize = 624;
pub(super) const HEAD_BOW_BASE: usize = 640;
pub(super) const FACE_GLASSES_BASE: usize = 656;
pub(super) const FACE_SUNGLASSES_BASE: usize = 672;
pub(super) const FACE_SUNGLASSES_TRIM: usize = 688;
pub(super) const NECK_BOW_TIE_BASE: usize = 704;
pub(super) const NECK_SCARF_BASE: usize = 720;
pub(super) const HEAD_BEANIE_DANGLE_TRIM: usize = 736;
pub(super) const HEAD_SPRIG_DANGLE_TRIM: usize = 752;
pub(super) const HEAD_ANTENNA_DANGLE_BASE: usize = 768;
pub(super) const HEAD_ANTENNA_DANGLE_TRIM: usize = 784;
pub(super) const LAPTOP: usize = 800;
pub(super) const NOTEPAD: usize = 805;
pub(super) const FX_SPARK: usize = 810;
pub(super) const FX_BUBBLE: usize = 811;
pub(super) const FX_DROP: usize = 812;
pub(super) const FX_NOTE: usize = 813;
pub(super) const FX_PUFF: usize = 814;
pub(super) const FX_DOT: usize = 815;
pub(super) const FX_DOT_LIT: usize = 816;
pub(super) const FX_BANG: usize = 817;
pub(super) const FX_STAR: usize = 818;
pub(super) const FX_HEART: usize = 819;
pub(super) const FX_BULB: usize = 820;
pub(super) const FX_BULB_LIT: usize = 821;
pub(super) const FX_SPEED: usize = 822;
pub(super) const FX_Z: usize = 823;
pub(super) const CONFETTI: usize = 824;
pub(super) const SPROUT: usize = 828;
pub(super) const KART_FRONT: usize = 833;
pub(super) const KART_BACK: usize = 840;
pub(super) const WHEEL: usize = 847;
pub(super) const SHADOW: usize = 855;

/// 拼装用的位置（pt）。`FEET` 是脚底在画布上的位置；身上各处相对脚底；`TIP_*` 是道具
/// 朝上时尖端相对握点；`NOTEPAD_AT` 相对左手；`ARM_LEN` 是肩到手。`SHOULDERS` 是每个朝向
/// 左、右肩的位置和比身子中轴离镜头远多少（正的在身子后面）。
pub(super) const CANVAS: f32 = 36.0;
pub(super) const FEET: (f32, f32) = (18.0, 28.605);
pub(super) const SHOULDERS: [[(f32, f32, f32); 2]; 16] = [
    [(-11.06, -9.76, 0.275), (9.9, -8.305, -4.745)],
    [(-10.325, -8.575, -3.815), (11.035, -9.465, -0.745)],
    [(-8.02, -7.56, -7.32), (10.495, -10.655, 3.365)],
    [(-4.495, -6.865, -9.71), (8.355, -11.7, 6.965)],
    [(-0.285, -6.6, -10.62), (4.94, -12.44, 9.505)],
    [(3.97, -6.805, -9.92), (0.775, -12.755, 10.6)],
    [(7.62, -7.445, -7.705), (-3.505, -12.605, 10.08)],
    [(10.11, -8.43, -4.315), (-7.255, -12.01, 8.025)],
    [(11.06, -9.6, -0.275), (-9.9, -11.055, 4.745)],
    [(10.325, -10.785, 3.815), (-11.035, -9.9, 0.745)],
    [(8.02, -11.805, 7.32), (-10.495, -8.705, -3.365)],
    [(4.495, -12.495, 9.71), (-8.355, -7.66, -6.965)],
    [(0.285, -12.76, 10.62), (-4.94, -6.925, -9.505)],
    [(-3.97, -12.555, 9.92), (-0.775, -6.605, -10.6)],
    [(-7.62, -11.915, 7.705), (3.505, -6.76, -10.08)],
    [(-10.11, -10.935, 4.315), (7.255, -7.355, -8.025)],
];
pub(super) const ARM_LEN: f32 = 4.8;
pub(super) const OVERHEAD: (f32, f32) = (0.0, -29.045);
pub(super) const MOUTH: (f32, f32) = (-2.61, -4.69);
pub(super) const LAPTOP_AT: (f32, f32) = (-3.83, 2.9);
pub(super) const NOTEPAD_AT: (f32, f32) = (0.935, -0.305);
pub(super) const SPROUT_AT: (f32, f32) = (14.56, 3.08);
pub(super) const WHEEL_AT: (f32, f32) = (-1.74, -1.01);
pub(super) const TYRE_L: (f32, f32) = (-11.415, -3.185);
pub(super) const TYRE_R: (f32, f32) = (14.665, -1.37);
pub(super) const TIP_WRENCH: (f32, f32) = (0.0, -11.525);
pub(super) const TIP_MAGNIFIER: (f32, f32) = (0.0, -9.22);
pub(super) const TIP_PENCIL: (f32, f32) = (0.0, -9.335);
pub(super) const TIP_FLASK: (f32, f32) = (-0.23, -10.345);
pub(super) const TIP_CAN: (f32, f32) = (6.87, 2.23);
pub(super) const TIP_BATON: (f32, f32) = (0.0, -10.835);
pub(super) const TIP_BROOM: (f32, f32) = (0.0, -15.675);

pub(super) static SPRITES: [Sprite; 856] = [
    Sprite {
        start: 0,
        len: 1594,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 1594,
        len: 1532,
        anchor: (12.71, 23.16),
    },
    Sprite {
        start: 3126,
        len: 1526,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 4652,
        len: 1530,
        anchor: (11.98, 23.205),
    },
    Sprite {
        start: 6182,
        len: 1482,
        anchor: (11.615, 23.22),
    },
    Sprite {
        start: 7664,
        len: 1430,
        anchor: (11.565, 23.225),
    },
    Sprite {
        start: 9094,
        len: 1532,
        anchor: (11.875, 23.21),
    },
    Sprite {
        start: 10626,
        len: 1376,
        anchor: (12.335, 23.185),
    },
    Sprite {
        start: 12002,
        len: 1310,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 13312,
        len: 1320,
        anchor: (12.71, 23.16),
    },
    Sprite {
        start: 14632,
        len: 1390,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 16022,
        len: 1414,
        anchor: (12.465, 23.205),
    },
    Sprite {
        start: 17436,
        len: 1342,
        anchor: (11.985, 23.22),
    },
    Sprite {
        start: 18778,
        len: 1366,
        anchor: (11.65, 23.225),
    },
    Sprite {
        start: 20144,
        len: 1536,
        anchor: (12.485, 23.21),
    },
    Sprite {
        start: 21680,
        len: 1534,
        anchor: (12.335, 23.185),
    },
    Sprite {
        start: 23214,
        len: 538,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 23752,
        len: 500,
        anchor: (12.71, 23.16),
    },
    Sprite {
        start: 24252,
        len: 562,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 24814,
        len: 446,
        anchor: (11.98, 23.205),
    },
    Sprite {
        start: 25260,
        len: 310,
        anchor: (11.615, 23.22),
    },
    Sprite {
        start: 25570,
        len: 274,
        anchor: (11.565, 23.225),
    },
    Sprite {
        start: 25844,
        len: 282,
        anchor: (11.875, 23.21),
    },
    Sprite {
        start: 26126,
        len: 94,
        anchor: (12.335, 23.185),
    },
    Sprite {
        start: 26220,
        len: 94,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 26314,
        len: 94,
        anchor: (12.71, 23.16),
    },
    Sprite {
        start: 26408,
        len: 94,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 26502,
        len: 198,
        anchor: (12.465, 23.205),
    },
    Sprite {
        start: 26700,
        len: 272,
        anchor: (11.985, 23.22),
    },
    Sprite {
        start: 26972,
        len: 286,
        anchor: (11.65, 23.225),
    },
    Sprite {
        start: 27258,
        len: 420,
        anchor: (12.485, 23.21),
    },
    Sprite {
        start: 27678,
        len: 502,
        anchor: (12.335, 23.185),
    },
    Sprite {
        start: 28180,
        len: 536,
        anchor: (6.72, 6.72),
    },
    Sprite {
        start: 28716,
        len: 462,
        anchor: (6.72, 6.72),
    },
    Sprite {
        start: 29178,
        len: 538,
        anchor: (6.72, 6.72),
    },
    Sprite {
        start: 29716,
        len: 500,
        anchor: (6.72, 6.72),
    },
    Sprite {
        start: 30216,
        len: 524,
        anchor: (6.72, 6.72),
    },
    Sprite {
        start: 30740,
        len: 496,
        anchor: (6.72, 6.72),
    },
    Sprite {
        start: 31236,
        len: 504,
        anchor: (6.72, 6.72),
    },
    Sprite {
        start: 31740,
        len: 486,
        anchor: (6.72, 6.72),
    },
    Sprite {
        start: 32226,
        len: 888,
        anchor: (14.555, 14.555),
    },
    Sprite {
        start: 33114,
        len: 1002,
        anchor: (14.555, 14.555),
    },
    Sprite {
        start: 34116,
        len: 876,
        anchor: (14.555, 14.555),
    },
    Sprite {
        start: 34992,
        len: 1054,
        anchor: (14.555, 14.555),
    },
    Sprite {
        start: 36046,
        len: 938,
        anchor: (14.555, 14.555),
    },
    Sprite {
        start: 36984,
        len: 1054,
        anchor: (14.555, 14.555),
    },
    Sprite {
        start: 38038,
        len: 824,
        anchor: (14.555, 14.555),
    },
    Sprite {
        start: 38862,
        len: 1020,
        anchor: (14.555, 14.555),
    },
    Sprite {
        start: 39882,
        len: 958,
        anchor: (14.615, 14.615),
    },
    Sprite {
        start: 40840,
        len: 1106,
        anchor: (14.615, 14.615),
    },
    Sprite {
        start: 41946,
        len: 1010,
        anchor: (14.615, 14.615),
    },
    Sprite {
        start: 42956,
        len: 1094,
        anchor: (14.615, 14.615),
    },
    Sprite {
        start: 44050,
        len: 1026,
        anchor: (14.615, 14.615),
    },
    Sprite {
        start: 45076,
        len: 1088,
        anchor: (14.615, 14.615),
    },
    Sprite {
        start: 46164,
        len: 944,
        anchor: (14.615, 14.615),
    },
    Sprite {
        start: 47108,
        len: 1094,
        anchor: (14.615, 14.615),
    },
    Sprite {
        start: 48202,
        len: 734,
        anchor: (10.055, 10.055),
    },
    Sprite {
        start: 48936,
        len: 740,
        anchor: (10.055, 10.055),
    },
    Sprite {
        start: 49676,
        len: 730,
        anchor: (10.055, 10.055),
    },
    Sprite {
        start: 50406,
        len: 664,
        anchor: (10.055, 10.055),
    },
    Sprite {
        start: 51070,
        len: 754,
        anchor: (10.055, 10.055),
    },
    Sprite {
        start: 51824,
        len: 718,
        anchor: (10.055, 10.055),
    },
    Sprite {
        start: 52542,
        len: 680,
        anchor: (10.055, 10.055),
    },
    Sprite {
        start: 53222,
        len: 632,
        anchor: (10.055, 10.055),
    },
    Sprite {
        start: 53854,
        len: 766,
        anchor: (11.22, 11.22),
    },
    Sprite {
        start: 54620,
        len: 778,
        anchor: (11.22, 11.22),
    },
    Sprite {
        start: 55398,
        len: 712,
        anchor: (11.22, 11.22),
    },
    Sprite {
        start: 56110,
        len: 774,
        anchor: (11.22, 11.22),
    },
    Sprite {
        start: 56884,
        len: 736,
        anchor: (11.22, 11.22),
    },
    Sprite {
        start: 57620,
        len: 752,
        anchor: (11.22, 11.22),
    },
    Sprite {
        start: 58372,
        len: 812,
        anchor: (11.22, 11.22),
    },
    Sprite {
        start: 59184,
        len: 808,
        anchor: (11.22, 11.22),
    },
    Sprite {
        start: 59992,
        len: 998,
        anchor: (9.045, 9.045),
    },
    Sprite {
        start: 60990,
        len: 970,
        anchor: (9.045, 9.045),
    },
    Sprite {
        start: 61960,
        len: 910,
        anchor: (9.045, 9.045),
    },
    Sprite {
        start: 62870,
        len: 996,
        anchor: (9.045, 9.045),
    },
    Sprite {
        start: 63866,
        len: 964,
        anchor: (9.045, 9.045),
    },
    Sprite {
        start: 64830,
        len: 964,
        anchor: (9.045, 9.045),
    },
    Sprite {
        start: 65794,
        len: 1022,
        anchor: (9.045, 9.045),
    },
    Sprite {
        start: 66816,
        len: 990,
        anchor: (9.045, 9.045),
    },
    Sprite {
        start: 67806,
        len: 416,
        anchor: (11.675, 11.675),
    },
    Sprite {
        start: 68222,
        len: 492,
        anchor: (11.675, 11.675),
    },
    Sprite {
        start: 68714,
        len: 394,
        anchor: (11.675, 11.675),
    },
    Sprite {
        start: 69108,
        len: 544,
        anchor: (11.675, 11.675),
    },
    Sprite {
        start: 69652,
        len: 426,
        anchor: (11.675, 11.675),
    },
    Sprite {
        start: 70078,
        len: 576,
        anchor: (11.675, 11.675),
    },
    Sprite {
        start: 70654,
        len: 406,
        anchor: (11.675, 11.675),
    },
    Sprite {
        start: 71060,
        len: 494,
        anchor: (11.675, 11.675),
    },
    Sprite {
        start: 71554,
        len: 882,
        anchor: (17.465, 17.465),
    },
    Sprite {
        start: 72436,
        len: 1172,
        anchor: (17.465, 17.465),
    },
    Sprite {
        start: 73608,
        len: 828,
        anchor: (17.465, 17.465),
    },
    Sprite {
        start: 74436,
        len: 1170,
        anchor: (17.465, 17.465),
    },
    Sprite {
        start: 75606,
        len: 854,
        anchor: (17.465, 17.465),
    },
    Sprite {
        start: 76460,
        len: 1198,
        anchor: (17.465, 17.465),
    },
    Sprite {
        start: 77658,
        len: 806,
        anchor: (17.465, 17.465),
    },
    Sprite {
        start: 78464,
        len: 1144,
        anchor: (17.465, 17.465),
    },
    Sprite {
        start: 79608,
        len: 414,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 80022,
        len: 404,
        anchor: (12.71, 23.16),
    },
    Sprite {
        start: 80426,
        len: 402,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 80828,
        len: 380,
        anchor: (11.98, 23.205),
    },
    Sprite {
        start: 81208,
        len: 408,
        anchor: (11.615, 23.22),
    },
    Sprite {
        start: 81616,
        len: 308,
        anchor: (11.565, 23.225),
    },
    Sprite {
        start: 81924,
        len: 278,
        anchor: (11.875, 23.21),
    },
    Sprite {
        start: 82202,
        len: 94,
        anchor: (12.335, 23.185),
    },
    Sprite {
        start: 82296,
        len: 94,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 82390,
        len: 94,
        anchor: (12.71, 23.16),
    },
    Sprite {
        start: 82484,
        len: 94,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 82578,
        len: 94,
        anchor: (12.465, 23.205),
    },
    Sprite {
        start: 82672,
        len: 92,
        anchor: (11.985, 23.22),
    },
    Sprite {
        start: 82764,
        len: 258,
        anchor: (12.05, 23.225),
    },
    Sprite {
        start: 83022,
        len: 274,
        anchor: (12.485, 23.21),
    },
    Sprite {
        start: 83296,
        len: 342,
        anchor: (12.335, 23.185),
    },
    Sprite {
        start: 83638,
        len: 398,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 84036,
        len: 394,
        anchor: (12.71, 23.16),
    },
    Sprite {
        start: 84430,
        len: 386,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 84816,
        len: 366,
        anchor: (11.98, 23.205),
    },
    Sprite {
        start: 85182,
        len: 256,
        anchor: (11.615, 23.22),
    },
    Sprite {
        start: 85438,
        len: 92,
        anchor: (11.565, 23.225),
    },
    Sprite {
        start: 85530,
        len: 94,
        anchor: (11.875, 23.21),
    },
    Sprite {
        start: 85624,
        len: 94,
        anchor: (12.335, 23.185),
    },
    Sprite {
        start: 85718,
        len: 94,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 85812,
        len: 94,
        anchor: (12.71, 23.16),
    },
    Sprite {
        start: 85906,
        len: 94,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 86000,
        len: 232,
        anchor: (12.465, 23.205),
    },
    Sprite {
        start: 86232,
        len: 332,
        anchor: (12.085, 23.22),
    },
    Sprite {
        start: 86564,
        len: 376,
        anchor: (11.65, 23.225),
    },
    Sprite {
        start: 86940,
        len: 394,
        anchor: (12.485, 23.21),
    },
    Sprite {
        start: 87334,
        len: 418,
        anchor: (12.335, 23.185),
    },
    Sprite {
        start: 87752,
        len: 322,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 88074,
        len: 294,
        anchor: (12.71, 23.16),
    },
    Sprite {
        start: 88368,
        len: 324,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 88692,
        len: 298,
        anchor: (11.98, 23.205),
    },
    Sprite {
        start: 88990,
        len: 268,
        anchor: (11.615, 23.22),
    },
    Sprite {
        start: 89258,
        len: 238,
        anchor: (11.565, 23.225),
    },
    Sprite {
        start: 89496,
        len: 290,
        anchor: (11.875, 23.21),
    },
    Sprite {
        start: 89786,
        len: 94,
        anchor: (12.335, 23.185),
    },
    Sprite {
        start: 89880,
        len: 94,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 89974,
        len: 94,
        anchor: (12.71, 23.16),
    },
    Sprite {
        start: 90068,
        len: 94,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 90162,
        len: 94,
        anchor: (12.465, 23.205),
    },
    Sprite {
        start: 90256,
        len: 92,
        anchor: (11.985, 23.22),
    },
    Sprite {
        start: 90348,
        len: 216,
        anchor: (12.165, 23.225),
    },
    Sprite {
        start: 90564,
        len: 224,
        anchor: (12.485, 23.21),
    },
    Sprite {
        start: 90788,
        len: 284,
        anchor: (12.335, 23.185),
    },
    Sprite {
        start: 91072,
        len: 320,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 91392,
        len: 330,
        anchor: (12.71, 23.16),
    },
    Sprite {
        start: 91722,
        len: 270,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 91992,
        len: 248,
        anchor: (11.98, 23.205),
    },
    Sprite {
        start: 92240,
        len: 224,
        anchor: (11.615, 23.22),
    },
    Sprite {
        start: 92464,
        len: 92,
        anchor: (11.565, 23.225),
    },
    Sprite {
        start: 92556,
        len: 94,
        anchor: (11.875, 23.21),
    },
    Sprite {
        start: 92650,
        len: 94,
        anchor: (12.335, 23.185),
    },
    Sprite {
        start: 92744,
        len: 94,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 92838,
        len: 94,
        anchor: (12.71, 23.16),
    },
    Sprite {
        start: 92932,
        len: 94,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 93026,
        len: 246,
        anchor: (12.465, 23.205),
    },
    Sprite {
        start: 93272,
        len: 238,
        anchor: (12.22, 23.22),
    },
    Sprite {
        start: 93510,
        len: 288,
        anchor: (11.65, 23.225),
    },
    Sprite {
        start: 93798,
        len: 306,
        anchor: (12.485, 23.21),
    },
    Sprite {
        start: 94104,
        len: 344,
        anchor: (12.335, 23.185),
    },
    Sprite {
        start: 94448,
        len: 218,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 94666,
        len: 270,
        anchor: (12.71, 23.16),
    },
    Sprite {
        start: 94936,
        len: 238,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 95174,
        len: 254,
        anchor: (11.98, 23.205),
    },
    Sprite {
        start: 95428,
        len: 268,
        anchor: (11.615, 23.22),
    },
    Sprite {
        start: 95696,
        len: 216,
        anchor: (11.565, 23.225),
    },
    Sprite {
        start: 95912,
        len: 232,
        anchor: (11.875, 23.21),
    },
    Sprite {
        start: 96144,
        len: 94,
        anchor: (12.335, 23.185),
    },
    Sprite {
        start: 96238,
        len: 94,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 96332,
        len: 94,
        anchor: (12.71, 23.16),
    },
    Sprite {
        start: 96426,
        len: 94,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 96520,
        len: 94,
        anchor: (12.465, 23.205),
    },
    Sprite {
        start: 96614,
        len: 92,
        anchor: (11.985, 23.22),
    },
    Sprite {
        start: 96706,
        len: 206,
        anchor: (12.095, 23.225),
    },
    Sprite {
        start: 96912,
        len: 214,
        anchor: (12.485, 23.21),
    },
    Sprite {
        start: 97126,
        len: 262,
        anchor: (12.335, 23.185),
    },
    Sprite {
        start: 97388,
        len: 240,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 97628,
        len: 238,
        anchor: (12.71, 23.16),
    },
    Sprite {
        start: 97866,
        len: 232,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 98098,
        len: 242,
        anchor: (11.98, 23.205),
    },
    Sprite {
        start: 98340,
        len: 184,
        anchor: (11.615, 23.22),
    },
    Sprite {
        start: 98524,
        len: 92,
        anchor: (11.565, 23.225),
    },
    Sprite {
        start: 98616,
        len: 94,
        anchor: (11.875, 23.21),
    },
    Sprite {
        start: 98710,
        len: 94,
        anchor: (12.335, 23.185),
    },
    Sprite {
        start: 98804,
        len: 94,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 98898,
        len: 94,
        anchor: (12.71, 23.16),
    },
    Sprite {
        start: 98992,
        len: 94,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 99086,
        len: 184,
        anchor: (12.465, 23.205),
    },
    Sprite {
        start: 99270,
        len: 200,
        anchor: (12.16, 23.22),
    },
    Sprite {
        start: 99470,
        len: 244,
        anchor: (11.65, 23.225),
    },
    Sprite {
        start: 99714,
        len: 222,
        anchor: (12.485, 23.21),
    },
    Sprite {
        start: 99936,
        len: 264,
        anchor: (12.335, 23.185),
    },
    Sprite {
        start: 100200,
        len: 328,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 100528,
        len: 386,
        anchor: (12.71, 23.16),
    },
    Sprite {
        start: 100914,
        len: 352,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 101266,
        len: 326,
        anchor: (11.98, 23.205),
    },
    Sprite {
        start: 101592,
        len: 324,
        anchor: (11.615, 23.22),
    },
    Sprite {
        start: 101916,
        len: 272,
        anchor: (11.565, 23.225),
    },
    Sprite {
        start: 102188,
        len: 270,
        anchor: (11.875, 23.21),
    },
    Sprite {
        start: 102458,
        len: 94,
        anchor: (12.335, 23.185),
    },
    Sprite {
        start: 102552,
        len: 94,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 102646,
        len: 94,
        anchor: (12.71, 23.16),
    },
    Sprite {
        start: 102740,
        len: 94,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 102834,
        len: 94,
        anchor: (12.465, 23.205),
    },
    Sprite {
        start: 102928,
        len: 92,
        anchor: (11.985, 23.22),
    },
    Sprite {
        start: 103020,
        len: 226,
        anchor: (12.135, 23.225),
    },
    Sprite {
        start: 103246,
        len: 274,
        anchor: (12.485, 23.21),
    },
    Sprite {
        start: 103520,
        len: 302,
        anchor: (12.335, 23.185),
    },
    Sprite {
        start: 103822,
        len: 332,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 104154,
        len: 384,
        anchor: (12.71, 23.16),
    },
    Sprite {
        start: 104538,
        len: 320,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 104858,
        len: 292,
        anchor: (11.98, 23.205),
    },
    Sprite {
        start: 105150,
        len: 218,
        anchor: (11.615, 23.22),
    },
    Sprite {
        start: 105368,
        len: 110,
        anchor: (11.565, 23.225),
    },
    Sprite {
        start: 105478,
        len: 94,
        anchor: (11.875, 23.21),
    },
    Sprite {
        start: 105572,
        len: 94,
        anchor: (12.335, 23.185),
    },
    Sprite {
        start: 105666,
        len: 94,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 105760,
        len: 94,
        anchor: (12.71, 23.16),
    },
    Sprite {
        start: 105854,
        len: 94,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 105948,
        len: 200,
        anchor: (12.465, 23.205),
    },
    Sprite {
        start: 106148,
        len: 242,
        anchor: (12.18, 23.22),
    },
    Sprite {
        start: 106390,
        len: 288,
        anchor: (11.65, 23.225),
    },
    Sprite {
        start: 106678,
        len: 320,
        anchor: (12.485, 23.21),
    },
    Sprite {
        start: 106998,
        len: 328,
        anchor: (12.335, 23.185),
    },
    Sprite {
        start: 107326,
        len: 466,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 107792,
        len: 490,
        anchor: (12.71, 23.16),
    },
    Sprite {
        start: 108282,
        len: 444,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 108726,
        len: 422,
        anchor: (11.98, 23.205),
    },
    Sprite {
        start: 109148,
        len: 388,
        anchor: (11.615, 23.22),
    },
    Sprite {
        start: 109536,
        len: 316,
        anchor: (11.565, 23.225),
    },
    Sprite {
        start: 109852,
        len: 300,
        anchor: (11.875, 23.21),
    },
    Sprite {
        start: 110152,
        len: 94,
        anchor: (12.335, 23.185),
    },
    Sprite {
        start: 110246,
        len: 94,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 110340,
        len: 94,
        anchor: (12.71, 23.16),
    },
    Sprite {
        start: 110434,
        len: 94,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 110528,
        len: 94,
        anchor: (12.465, 23.205),
    },
    Sprite {
        start: 110622,
        len: 92,
        anchor: (11.985, 23.22),
    },
    Sprite {
        start: 110714,
        len: 266,
        anchor: (12.105, 23.225),
    },
    Sprite {
        start: 110980,
        len: 308,
        anchor: (12.485, 23.21),
    },
    Sprite {
        start: 111288,
        len: 388,
        anchor: (12.335, 23.185),
    },
    Sprite {
        start: 111676,
        len: 460,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 112136,
        len: 450,
        anchor: (12.71, 23.16),
    },
    Sprite {
        start: 112586,
        len: 424,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 113010,
        len: 346,
        anchor: (11.98, 23.205),
    },
    Sprite {
        start: 113356,
        len: 262,
        anchor: (11.615, 23.22),
    },
    Sprite {
        start: 113618,
        len: 92,
        anchor: (11.565, 23.225),
    },
    Sprite {
        start: 113710,
        len: 94,
        anchor: (11.875, 23.21),
    },
    Sprite {
        start: 113804,
        len: 94,
        anchor: (12.335, 23.185),
    },
    Sprite {
        start: 113898,
        len: 94,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 113992,
        len: 94,
        anchor: (12.71, 23.16),
    },
    Sprite {
        start: 114086,
        len: 94,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 114180,
        len: 264,
        anchor: (12.465, 23.205),
    },
    Sprite {
        start: 114444,
        len: 296,
        anchor: (12.1, 23.22),
    },
    Sprite {
        start: 114740,
        len: 354,
        anchor: (11.65, 23.225),
    },
    Sprite {
        start: 115094,
        len: 462,
        anchor: (12.485, 23.21),
    },
    Sprite {
        start: 115556,
        len: 510,
        anchor: (12.335, 23.185),
    },
    Sprite {
        start: 116066,
        len: 172,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 116238,
        len: 180,
        anchor: (12.71, 23.16),
    },
    Sprite {
        start: 116418,
        len: 190,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 116608,
        len: 164,
        anchor: (11.98, 23.205),
    },
    Sprite {
        start: 116772,
        len: 160,
        anchor: (11.615, 23.22),
    },
    Sprite {
        start: 116932,
        len: 148,
        anchor: (11.565, 23.225),
    },
    Sprite {
        start: 117080,
        len: 94,
        anchor: (11.875, 23.21),
    },
    Sprite {
        start: 117174,
        len: 94,
        anchor: (12.335, 23.185),
    },
    Sprite {
        start: 117268,
        len: 94,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 117362,
        len: 94,
        anchor: (12.71, 23.16),
    },
    Sprite {
        start: 117456,
        len: 94,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 117550,
        len: 94,
        anchor: (12.465, 23.205),
    },
    Sprite {
        start: 117644,
        len: 158,
        anchor: (11.985, 23.22),
    },
    Sprite {
        start: 117802,
        len: 216,
        anchor: (11.84, 23.225),
    },
    Sprite {
        start: 118018,
        len: 186,
        anchor: (12.485, 23.21),
    },
    Sprite {
        start: 118204,
        len: 176,
        anchor: (12.335, 23.185),
    },
    Sprite {
        start: 118380,
        len: 202,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 118582,
        len: 168,
        anchor: (12.71, 23.16),
    },
    Sprite {
        start: 118750,
        len: 170,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 118920,
        len: 184,
        anchor: (11.98, 23.205),
    },
    Sprite {
        start: 119104,
        len: 200,
        anchor: (11.615, 23.22),
    },
    Sprite {
        start: 119304,
        len: 198,
        anchor: (11.565, 23.225),
    },
    Sprite {
        start: 119502,
        len: 94,
        anchor: (11.875, 23.21),
    },
    Sprite {
        start: 119596,
        len: 94,
        anchor: (12.335, 23.185),
    },
    Sprite {
        start: 119690,
        len: 94,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 119784,
        len: 94,
        anchor: (12.71, 23.16),
    },
    Sprite {
        start: 119878,
        len: 94,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 119972,
        len: 94,
        anchor: (12.465, 23.205),
    },
    Sprite {
        start: 120066,
        len: 136,
        anchor: (11.985, 23.22),
    },
    Sprite {
        start: 120202,
        len: 206,
        anchor: (11.835, 23.225),
    },
    Sprite {
        start: 120408,
        len: 188,
        anchor: (12.485, 23.21),
    },
    Sprite {
        start: 120596,
        len: 180,
        anchor: (12.335, 23.185),
    },
    Sprite {
        start: 120776,
        len: 260,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 121036,
        len: 254,
        anchor: (12.71, 23.16),
    },
    Sprite {
        start: 121290,
        len: 226,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 121516,
        len: 260,
        anchor: (11.98, 23.205),
    },
    Sprite {
        start: 121776,
        len: 232,
        anchor: (11.615, 23.22),
    },
    Sprite {
        start: 122008,
        len: 108,
        anchor: (11.565, 23.225),
    },
    Sprite {
        start: 122116,
        len: 94,
        anchor: (11.875, 23.21),
    },
    Sprite {
        start: 122210,
        len: 94,
        anchor: (12.335, 23.185),
    },
    Sprite {
        start: 122304,
        len: 94,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 122398,
        len: 94,
        anchor: (12.71, 23.16),
    },
    Sprite {
        start: 122492,
        len: 94,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 122586,
        len: 94,
        anchor: (12.465, 23.205),
    },
    Sprite {
        start: 122680,
        len: 92,
        anchor: (11.985, 23.22),
    },
    Sprite {
        start: 122772,
        len: 224,
        anchor: (11.65, 23.225),
    },
    Sprite {
        start: 122996,
        len: 272,
        anchor: (12.485, 23.21),
    },
    Sprite {
        start: 123268,
        len: 212,
        anchor: (12.335, 23.185),
    },
    Sprite {
        start: 123480,
        len: 194,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 123674,
        len: 196,
        anchor: (12.71, 23.16),
    },
    Sprite {
        start: 123870,
        len: 210,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 124080,
        len: 204,
        anchor: (11.98, 23.205),
    },
    Sprite {
        start: 124284,
        len: 182,
        anchor: (11.615, 23.22),
    },
    Sprite {
        start: 124466,
        len: 110,
        anchor: (11.565, 23.225),
    },
    Sprite {
        start: 124576,
        len: 94,
        anchor: (11.875, 23.21),
    },
    Sprite {
        start: 124670,
        len: 94,
        anchor: (12.335, 23.185),
    },
    Sprite {
        start: 124764,
        len: 94,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 124858,
        len: 94,
        anchor: (12.71, 23.16),
    },
    Sprite {
        start: 124952,
        len: 94,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 125046,
        len: 94,
        anchor: (12.465, 23.205),
    },
    Sprite {
        start: 125140,
        len: 92,
        anchor: (11.985, 23.22),
    },
    Sprite {
        start: 125232,
        len: 212,
        anchor: (11.65, 23.225),
    },
    Sprite {
        start: 125444,
        len: 218,
        anchor: (12.485, 23.21),
    },
    Sprite {
        start: 125662,
        len: 192,
        anchor: (12.335, 23.185),
    },
    Sprite {
        start: 125854,
        len: 358,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 126212,
        len: 326,
        anchor: (12.71, 23.16),
    },
    Sprite {
        start: 126538,
        len: 338,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 126876,
        len: 336,
        anchor: (11.98, 23.205),
    },
    Sprite {
        start: 127212,
        len: 288,
        anchor: (11.615, 23.22),
    },
    Sprite {
        start: 127500,
        len: 182,
        anchor: (11.565, 23.225),
    },
    Sprite {
        start: 127682,
        len: 94,
        anchor: (11.875, 23.21),
    },
    Sprite {
        start: 127776,
        len: 94,
        anchor: (12.335, 23.185),
    },
    Sprite {
        start: 127870,
        len: 94,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 127964,
        len: 94,
        anchor: (12.71, 23.16),
    },
    Sprite {
        start: 128058,
        len: 94,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 128152,
        len: 94,
        anchor: (12.465, 23.205),
    },
    Sprite {
        start: 128246,
        len: 92,
        anchor: (11.985, 23.22),
    },
    Sprite {
        start: 128338,
        len: 244,
        anchor: (11.65, 23.225),
    },
    Sprite {
        start: 128582,
        len: 332,
        anchor: (12.485, 23.21),
    },
    Sprite {
        start: 128914,
        len: 312,
        anchor: (12.335, 23.185),
    },
    Sprite {
        start: 129226,
        len: 1030,
        anchor: (12.675, 23.76),
    },
    Sprite {
        start: 130256,
        len: 1040,
        anchor: (12.71, 23.76),
    },
    Sprite {
        start: 131296,
        len: 1036,
        anchor: (12.435, 23.775),
    },
    Sprite {
        start: 132332,
        len: 1008,
        anchor: (11.98, 23.8),
    },
    Sprite {
        start: 133340,
        len: 974,
        anchor: (11.615, 23.82),
    },
    Sprite {
        start: 134314,
        len: 984,
        anchor: (11.565, 23.82),
    },
    Sprite {
        start: 135298,
        len: 1042,
        anchor: (11.875, 23.805),
    },
    Sprite {
        start: 136340,
        len: 1054,
        anchor: (12.335, 23.78),
    },
    Sprite {
        start: 137394,
        len: 1016,
        anchor: (12.675, 23.76),
    },
    Sprite {
        start: 138410,
        len: 1068,
        anchor: (12.71, 23.76),
    },
    Sprite {
        start: 139478,
        len: 1066,
        anchor: (12.435, 23.775),
    },
    Sprite {
        start: 140544,
        len: 1056,
        anchor: (12.465, 23.8),
    },
    Sprite {
        start: 141600,
        len: 984,
        anchor: (11.985, 23.82),
    },
    Sprite {
        start: 142584,
        len: 1002,
        anchor: (11.65, 23.82),
    },
    Sprite {
        start: 143586,
        len: 1026,
        anchor: (12.485, 23.805),
    },
    Sprite {
        start: 144612,
        len: 1068,
        anchor: (12.335, 23.78),
    },
    Sprite {
        start: 145680,
        len: 998,
        anchor: (12.675, 23.925),
    },
    Sprite {
        start: 146678,
        len: 964,
        anchor: (12.71, 23.925),
    },
    Sprite {
        start: 147642,
        len: 1040,
        anchor: (12.435, 23.925),
    },
    Sprite {
        start: 148682,
        len: 992,
        anchor: (11.98, 23.925),
    },
    Sprite {
        start: 149674,
        len: 1074,
        anchor: (11.615, 23.925),
    },
    Sprite {
        start: 150748,
        len: 1032,
        anchor: (11.565, 23.925),
    },
    Sprite {
        start: 151780,
        len: 1016,
        anchor: (11.875, 23.925),
    },
    Sprite {
        start: 152796,
        len: 990,
        anchor: (12.335, 23.925),
    },
    Sprite {
        start: 153786,
        len: 870,
        anchor: (12.675, 23.925),
    },
    Sprite {
        start: 154656,
        len: 896,
        anchor: (12.71, 23.925),
    },
    Sprite {
        start: 155552,
        len: 958,
        anchor: (12.435, 23.925),
    },
    Sprite {
        start: 156510,
        len: 1044,
        anchor: (14.47, 23.925),
    },
    Sprite {
        start: 157554,
        len: 954,
        anchor: (15.77, 23.925),
    },
    Sprite {
        start: 158508,
        len: 1062,
        anchor: (15.91, 23.925),
    },
    Sprite {
        start: 159570,
        len: 1060,
        anchor: (14.905, 23.925),
    },
    Sprite {
        start: 160630,
        len: 998,
        anchor: (12.62, 23.925),
    },
    Sprite {
        start: 161628,
        len: 1284,
        anchor: (12.675, 27.07),
    },
    Sprite {
        start: 162912,
        len: 1266,
        anchor: (12.71, 27.06),
    },
    Sprite {
        start: 164178,
        len: 1240,
        anchor: (12.435, 27.105),
    },
    Sprite {
        start: 165418,
        len: 1260,
        anchor: (11.98, 27.175),
    },
    Sprite {
        start: 166678,
        len: 1238,
        anchor: (11.615, 27.225),
    },
    Sprite {
        start: 167916,
        len: 1264,
        anchor: (11.565, 27.235),
    },
    Sprite {
        start: 169180,
        len: 1250,
        anchor: (11.875, 27.195),
    },
    Sprite {
        start: 170430,
        len: 1266,
        anchor: (12.335, 27.12),
    },
    Sprite {
        start: 171696,
        len: 1312,
        anchor: (12.675, 27.07),
    },
    Sprite {
        start: 173008,
        len: 1218,
        anchor: (12.71, 27.06),
    },
    Sprite {
        start: 174226,
        len: 1314,
        anchor: (12.435, 27.105),
    },
    Sprite {
        start: 175540,
        len: 1236,
        anchor: (12.465, 27.175),
    },
    Sprite {
        start: 176776,
        len: 1216,
        anchor: (11.985, 27.225),
    },
    Sprite {
        start: 177992,
        len: 1250,
        anchor: (11.65, 27.235),
    },
    Sprite {
        start: 179242,
        len: 1244,
        anchor: (12.485, 27.195),
    },
    Sprite {
        start: 180486,
        len: 1246,
        anchor: (12.335, 27.12),
    },
    Sprite {
        start: 181732,
        len: 578,
        anchor: (12.675, 27.07),
    },
    Sprite {
        start: 182310,
        len: 556,
        anchor: (12.71, 27.06),
    },
    Sprite {
        start: 182866,
        len: 544,
        anchor: (12.435, 27.105),
    },
    Sprite {
        start: 183410,
        len: 500,
        anchor: (11.98, 27.175),
    },
    Sprite {
        start: 183910,
        len: 534,
        anchor: (11.615, 27.225),
    },
    Sprite {
        start: 184444,
        len: 506,
        anchor: (11.565, 27.235),
    },
    Sprite {
        start: 184950,
        len: 530,
        anchor: (11.875, 27.195),
    },
    Sprite {
        start: 185480,
        len: 522,
        anchor: (12.335, 27.12),
    },
    Sprite {
        start: 186002,
        len: 590,
        anchor: (12.675, 27.07),
    },
    Sprite {
        start: 186592,
        len: 544,
        anchor: (12.71, 27.06),
    },
    Sprite {
        start: 187136,
        len: 528,
        anchor: (12.435, 27.105),
    },
    Sprite {
        start: 187664,
        len: 500,
        anchor: (12.465, 27.175),
    },
    Sprite {
        start: 188164,
        len: 510,
        anchor: (11.985, 27.225),
    },
    Sprite {
        start: 188674,
        len: 520,
        anchor: (11.65, 27.235),
    },
    Sprite {
        start: 189194,
        len: 468,
        anchor: (12.485, 27.195),
    },
    Sprite {
        start: 189662,
        len: 550,
        anchor: (12.335, 27.12),
    },
    Sprite {
        start: 190212,
        len: 906,
        anchor: (12.675, 26.335),
    },
    Sprite {
        start: 191118,
        len: 764,
        anchor: (12.71, 25.925),
    },
    Sprite {
        start: 191882,
        len: 804,
        anchor: (12.435, 25.475),
    },
    Sprite {
        start: 192686,
        len: 782,
        anchor: (11.98, 25.565),
    },
    Sprite {
        start: 193468,
        len: 746,
        anchor: (11.615, 25.56),
    },
    Sprite {
        start: 194214,
        len: 752,
        anchor: (11.565, 25.505),
    },
    Sprite {
        start: 194966,
        len: 778,
        anchor: (11.875, 25.435),
    },
    Sprite {
        start: 195744,
        len: 802,
        anchor: (12.335, 25.29),
    },
    Sprite {
        start: 196546,
        len: 758,
        anchor: (12.675, 25.56),
    },
    Sprite {
        start: 197304,
        len: 858,
        anchor: (12.71, 25.985),
    },
    Sprite {
        start: 198162,
        len: 864,
        anchor: (12.435, 26.375),
    },
    Sprite {
        start: 199026,
        len: 902,
        anchor: (12.465, 26.675),
    },
    Sprite {
        start: 199928,
        len: 836,
        anchor: (11.985, 26.88),
    },
    Sprite {
        start: 200764,
        len: 814,
        anchor: (11.65, 26.945),
    },
    Sprite {
        start: 201578,
        len: 926,
        anchor: (12.485, 26.87),
    },
    Sprite {
        start: 202504,
        len: 812,
        anchor: (12.335, 26.665),
    },
    Sprite {
        start: 203316,
        len: 830,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 204146,
        len: 788,
        anchor: (12.71, 23.16),
    },
    Sprite {
        start: 204934,
        len: 820,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 205754,
        len: 652,
        anchor: (11.98, 23.205),
    },
    Sprite {
        start: 206406,
        len: 542,
        anchor: (11.615, 23.22),
    },
    Sprite {
        start: 206948,
        len: 478,
        anchor: (11.565, 23.225),
    },
    Sprite {
        start: 207426,
        len: 426,
        anchor: (11.875, 23.21),
    },
    Sprite {
        start: 207852,
        len: 314,
        anchor: (12.335, 23.185),
    },
    Sprite {
        start: 208166,
        len: 94,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 208260,
        len: 94,
        anchor: (12.71, 23.16),
    },
    Sprite {
        start: 208354,
        len: 324,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 208678,
        len: 404,
        anchor: (12.465, 23.205),
    },
    Sprite {
        start: 209082,
        len: 432,
        anchor: (11.985, 23.22),
    },
    Sprite {
        start: 209514,
        len: 450,
        anchor: (11.65, 23.225),
    },
    Sprite {
        start: 209964,
        len: 606,
        anchor: (12.485, 23.21),
    },
    Sprite {
        start: 210570,
        len: 746,
        anchor: (12.335, 23.185),
    },
    Sprite {
        start: 211316,
        len: 1192,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 212508,
        len: 1146,
        anchor: (12.71, 23.16),
    },
    Sprite {
        start: 213654,
        len: 1142,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 214796,
        len: 1072,
        anchor: (11.98, 23.205),
    },
    Sprite {
        start: 215868,
        len: 960,
        anchor: (11.615, 23.22),
    },
    Sprite {
        start: 216828,
        len: 962,
        anchor: (11.565, 23.225),
    },
    Sprite {
        start: 217790,
        len: 902,
        anchor: (11.875, 23.21),
    },
    Sprite {
        start: 218692,
        len: 986,
        anchor: (12.335, 23.185),
    },
    Sprite {
        start: 219678,
        len: 986,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 220664,
        len: 954,
        anchor: (12.71, 23.16),
    },
    Sprite {
        start: 221618,
        len: 1000,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 222618,
        len: 956,
        anchor: (12.465, 23.205),
    },
    Sprite {
        start: 223574,
        len: 930,
        anchor: (11.985, 23.22),
    },
    Sprite {
        start: 224504,
        len: 940,
        anchor: (11.65, 23.225),
    },
    Sprite {
        start: 225444,
        len: 1042,
        anchor: (12.485, 23.21),
    },
    Sprite {
        start: 226486,
        len: 1118,
        anchor: (12.335, 23.185),
    },
    Sprite {
        start: 227604,
        len: 732,
        anchor: (14.605, 25.56),
    },
    Sprite {
        start: 228336,
        len: 752,
        anchor: (14.64, 25.57),
    },
    Sprite {
        start: 229088,
        len: 772,
        anchor: (14.605, 25.56),
    },
    Sprite {
        start: 229860,
        len: 748,
        anchor: (14.64, 25.57),
    },
    Sprite {
        start: 230608,
        len: 786,
        anchor: (14.605, 25.56),
    },
    Sprite {
        start: 231394,
        len: 816,
        anchor: (14.64, 25.57),
    },
    Sprite {
        start: 232210,
        len: 778,
        anchor: (14.605, 25.56),
    },
    Sprite {
        start: 232988,
        len: 766,
        anchor: (14.64, 25.57),
    },
    Sprite {
        start: 233754,
        len: 748,
        anchor: (14.605, 25.56),
    },
    Sprite {
        start: 234502,
        len: 748,
        anchor: (14.64, 25.57),
    },
    Sprite {
        start: 235250,
        len: 750,
        anchor: (14.605, 25.56),
    },
    Sprite {
        start: 236000,
        len: 760,
        anchor: (14.64, 25.57),
    },
    Sprite {
        start: 236760,
        len: 778,
        anchor: (14.605, 25.56),
    },
    Sprite {
        start: 237538,
        len: 794,
        anchor: (14.64, 25.57),
    },
    Sprite {
        start: 238332,
        len: 810,
        anchor: (14.605, 25.56),
    },
    Sprite {
        start: 239142,
        len: 778,
        anchor: (14.64, 25.57),
    },
    Sprite {
        start: 239920,
        len: 1434,
        anchor: (14.605, 25.56),
    },
    Sprite {
        start: 241354,
        len: 1462,
        anchor: (14.64, 25.57),
    },
    Sprite {
        start: 242816,
        len: 1464,
        anchor: (14.605, 25.56),
    },
    Sprite {
        start: 244280,
        len: 1456,
        anchor: (14.64, 25.57),
    },
    Sprite {
        start: 245736,
        len: 1436,
        anchor: (14.605, 25.56),
    },
    Sprite {
        start: 247172,
        len: 1482,
        anchor: (14.64, 25.57),
    },
    Sprite {
        start: 248654,
        len: 1458,
        anchor: (14.605, 25.56),
    },
    Sprite {
        start: 250112,
        len: 1498,
        anchor: (14.64, 25.57),
    },
    Sprite {
        start: 251610,
        len: 1464,
        anchor: (14.605, 25.56),
    },
    Sprite {
        start: 253074,
        len: 1424,
        anchor: (14.64, 25.57),
    },
    Sprite {
        start: 254498,
        len: 1466,
        anchor: (14.605, 25.56),
    },
    Sprite {
        start: 255964,
        len: 1458,
        anchor: (14.64, 25.57),
    },
    Sprite {
        start: 257422,
        len: 1454,
        anchor: (14.605, 25.56),
    },
    Sprite {
        start: 258876,
        len: 1476,
        anchor: (14.64, 25.57),
    },
    Sprite {
        start: 260352,
        len: 1462,
        anchor: (14.605, 25.56),
    },
    Sprite {
        start: 261814,
        len: 1456,
        anchor: (14.64, 25.57),
    },
    Sprite {
        start: 263270,
        len: 1150,
        anchor: (12.675, 23.595),
    },
    Sprite {
        start: 264420,
        len: 1058,
        anchor: (12.71, 23.59),
    },
    Sprite {
        start: 265478,
        len: 1044,
        anchor: (12.435, 23.61),
    },
    Sprite {
        start: 266522,
        len: 1016,
        anchor: (12.165, 23.635),
    },
    Sprite {
        start: 267538,
        len: 1032,
        anchor: (11.805, 23.65),
    },
    Sprite {
        start: 268570,
        len: 1096,
        anchor: (11.755, 23.655),
    },
    Sprite {
        start: 269666,
        len: 1138,
        anchor: (12.815, 23.64),
    },
    Sprite {
        start: 270804,
        len: 1158,
        anchor: (13.745, 23.615),
    },
    Sprite {
        start: 271962,
        len: 1154,
        anchor: (12.86, 23.595),
    },
    Sprite {
        start: 273116,
        len: 1204,
        anchor: (12.71, 23.59),
    },
    Sprite {
        start: 274320,
        len: 1140,
        anchor: (12.435, 23.61),
    },
    Sprite {
        start: 275460,
        len: 1166,
        anchor: (12.465, 23.635),
    },
    Sprite {
        start: 276626,
        len: 1098,
        anchor: (11.985, 23.65),
    },
    Sprite {
        start: 277724,
        len: 1116,
        anchor: (11.65, 23.655),
    },
    Sprite {
        start: 278840,
        len: 1110,
        anchor: (12.485, 23.64),
    },
    Sprite {
        start: 279950,
        len: 1128,
        anchor: (12.335, 23.615),
    },
    Sprite {
        start: 281078,
        len: 402,
        anchor: (12.675, 23.595),
    },
    Sprite {
        start: 281480,
        len: 376,
        anchor: (12.71, 23.59),
    },
    Sprite {
        start: 281856,
        len: 344,
        anchor: (12.435, 23.61),
    },
    Sprite {
        start: 282200,
        len: 320,
        anchor: (12.165, 23.635),
    },
    Sprite {
        start: 282520,
        len: 302,
        anchor: (11.805, 23.65),
    },
    Sprite {
        start: 282822,
        len: 208,
        anchor: (11.755, 23.655),
    },
    Sprite {
        start: 283030,
        len: 190,
        anchor: (12.815, 23.64),
    },
    Sprite {
        start: 283220,
        len: 94,
        anchor: (13.745, 23.615),
    },
    Sprite {
        start: 283314,
        len: 94,
        anchor: (12.86, 23.595),
    },
    Sprite {
        start: 283408,
        len: 94,
        anchor: (12.71, 23.59),
    },
    Sprite {
        start: 283502,
        len: 148,
        anchor: (12.435, 23.61),
    },
    Sprite {
        start: 283650,
        len: 198,
        anchor: (12.465, 23.635),
    },
    Sprite {
        start: 283848,
        len: 192,
        anchor: (11.985, 23.65),
    },
    Sprite {
        start: 284040,
        len: 244,
        anchor: (11.65, 23.655),
    },
    Sprite {
        start: 284284,
        len: 324,
        anchor: (12.485, 23.64),
    },
    Sprite {
        start: 284608,
        len: 396,
        anchor: (12.335, 23.615),
    },
    Sprite {
        start: 285004,
        len: 1270,
        anchor: (13.24, 24.29),
    },
    Sprite {
        start: 286274,
        len: 1308,
        anchor: (13.275, 24.3),
    },
    Sprite {
        start: 287582,
        len: 1248,
        anchor: (13.025, 24.215),
    },
    Sprite {
        start: 288830,
        len: 1190,
        anchor: (12.685, 24.16),
    },
    Sprite {
        start: 290020,
        len: 1186,
        anchor: (12.32, 24.18),
    },
    Sprite {
        start: 291206,
        len: 1270,
        anchor: (12.315, 24.18),
    },
    Sprite {
        start: 292476,
        len: 1426,
        anchor: (12.58, 24.165),
    },
    Sprite {
        start: 293902,
        len: 1564,
        anchor: (12.93, 24.185),
    },
    Sprite {
        start: 295466,
        len: 1600,
        anchor: (13.24, 24.29),
    },
    Sprite {
        start: 297066,
        len: 1610,
        anchor: (13.275, 24.3),
    },
    Sprite {
        start: 298676,
        len: 1598,
        anchor: (13.025, 24.215),
    },
    Sprite {
        start: 300274,
        len: 1500,
        anchor: (12.87, 24.16),
    },
    Sprite {
        start: 301774,
        len: 1448,
        anchor: (12.9, 24.18),
    },
    Sprite {
        start: 303222,
        len: 1258,
        anchor: (12.955, 24.18),
    },
    Sprite {
        start: 304480,
        len: 1218,
        anchor: (12.97, 24.165),
    },
    Sprite {
        start: 305698,
        len: 1238,
        anchor: (12.93, 24.185),
    },
    Sprite {
        start: 306936,
        len: 1626,
        anchor: (13.24, 24.29),
    },
    Sprite {
        start: 308562,
        len: 1674,
        anchor: (13.275, 24.3),
    },
    Sprite {
        start: 310236,
        len: 1612,
        anchor: (13.025, 24.215),
    },
    Sprite {
        start: 311848,
        len: 1610,
        anchor: (12.685, 24.16),
    },
    Sprite {
        start: 313458,
        len: 1588,
        anchor: (12.32, 24.18),
    },
    Sprite {
        start: 315046,
        len: 1576,
        anchor: (12.315, 24.18),
    },
    Sprite {
        start: 316622,
        len: 1604,
        anchor: (12.58, 24.165),
    },
    Sprite {
        start: 318226,
        len: 1532,
        anchor: (12.93, 24.185),
    },
    Sprite {
        start: 319758,
        len: 1426,
        anchor: (13.24, 24.29),
    },
    Sprite {
        start: 321184,
        len: 1384,
        anchor: (13.275, 24.3),
    },
    Sprite {
        start: 322568,
        len: 1516,
        anchor: (13.025, 24.215),
    },
    Sprite {
        start: 324084,
        len: 1600,
        anchor: (12.87, 24.16),
    },
    Sprite {
        start: 325684,
        len: 1604,
        anchor: (12.9, 24.18),
    },
    Sprite {
        start: 327288,
        len: 1582,
        anchor: (12.955, 24.18),
    },
    Sprite {
        start: 328870,
        len: 1502,
        anchor: (12.97, 24.165),
    },
    Sprite {
        start: 330372,
        len: 1540,
        anchor: (12.93, 24.185),
    },
    Sprite {
        start: 331912,
        len: 716,
        anchor: (12.675, 23.45),
    },
    Sprite {
        start: 332628,
        len: 786,
        anchor: (12.71, 23.45),
    },
    Sprite {
        start: 333414,
        len: 774,
        anchor: (12.435, 23.465),
    },
    Sprite {
        start: 334188,
        len: 792,
        anchor: (11.98, 23.49),
    },
    Sprite {
        start: 334980,
        len: 796,
        anchor: (11.615, 23.975),
    },
    Sprite {
        start: 335776,
        len: 870,
        anchor: (11.565, 24.725),
    },
    Sprite {
        start: 336646,
        len: 850,
        anchor: (11.875, 25.23),
    },
    Sprite {
        start: 337496,
        len: 846,
        anchor: (12.335, 25.47),
    },
    Sprite {
        start: 338342,
        len: 812,
        anchor: (12.675, 25.365),
    },
    Sprite {
        start: 339154,
        len: 878,
        anchor: (12.71, 24.985),
    },
    Sprite {
        start: 340032,
        len: 756,
        anchor: (12.435, 24.4),
    },
    Sprite {
        start: 340788,
        len: 746,
        anchor: (12.465, 23.665),
    },
    Sprite {
        start: 341534,
        len: 778,
        anchor: (11.985, 23.51),
    },
    Sprite {
        start: 342312,
        len: 744,
        anchor: (11.65, 23.51),
    },
    Sprite {
        start: 343056,
        len: 662,
        anchor: (12.485, 23.495),
    },
    Sprite {
        start: 343718,
        len: 714,
        anchor: (12.335, 23.47),
    },
    Sprite {
        start: 344432,
        len: 588,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 345020,
        len: 518,
        anchor: (12.71, 23.16),
    },
    Sprite {
        start: 345538,
        len: 476,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 346014,
        len: 396,
        anchor: (11.98, 23.205),
    },
    Sprite {
        start: 346410,
        len: 406,
        anchor: (11.615, 23.22),
    },
    Sprite {
        start: 346816,
        len: 304,
        anchor: (11.565, 23.225),
    },
    Sprite {
        start: 347120,
        len: 138,
        anchor: (11.875, 23.21),
    },
    Sprite {
        start: 347258,
        len: 178,
        anchor: (12.335, 23.185),
    },
    Sprite {
        start: 347436,
        len: 378,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 347814,
        len: 372,
        anchor: (12.71, 23.16),
    },
    Sprite {
        start: 348186,
        len: 448,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 348634,
        len: 428,
        anchor: (12.465, 23.205),
    },
    Sprite {
        start: 349062,
        len: 518,
        anchor: (11.985, 23.22),
    },
    Sprite {
        start: 349580,
        len: 592,
        anchor: (11.65, 23.225),
    },
    Sprite {
        start: 350172,
        len: 642,
        anchor: (12.485, 23.21),
    },
    Sprite {
        start: 350814,
        len: 608,
        anchor: (12.335, 23.185),
    },
    Sprite {
        start: 351422,
        len: 374,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 351796,
        len: 358,
        anchor: (12.71, 23.16),
    },
    Sprite {
        start: 352154,
        len: 318,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 352472,
        len: 288,
        anchor: (11.98, 23.205),
    },
    Sprite {
        start: 352760,
        len: 92,
        anchor: (11.615, 23.22),
    },
    Sprite {
        start: 352852,
        len: 92,
        anchor: (11.565, 23.225),
    },
    Sprite {
        start: 352944,
        len: 94,
        anchor: (11.875, 23.21),
    },
    Sprite {
        start: 353038,
        len: 94,
        anchor: (12.335, 23.185),
    },
    Sprite {
        start: 353132,
        len: 94,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 353226,
        len: 94,
        anchor: (12.71, 23.16),
    },
    Sprite {
        start: 353320,
        len: 302,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 353622,
        len: 342,
        anchor: (12.465, 23.205),
    },
    Sprite {
        start: 353964,
        len: 388,
        anchor: (11.985, 23.22),
    },
    Sprite {
        start: 354352,
        len: 378,
        anchor: (11.65, 23.225),
    },
    Sprite {
        start: 354730,
        len: 390,
        anchor: (12.485, 23.21),
    },
    Sprite {
        start: 355120,
        len: 404,
        anchor: (12.335, 23.185),
    },
    Sprite {
        start: 355524,
        len: 682,
        anchor: (15.46, 23.53),
    },
    Sprite {
        start: 356206,
        len: 722,
        anchor: (15.625, 23.5),
    },
    Sprite {
        start: 356928,
        len: 742,
        anchor: (14.475, 23.635),
    },
    Sprite {
        start: 357670,
        len: 482,
        anchor: (11.98, 23.875),
    },
    Sprite {
        start: 358152,
        len: 538,
        anchor: (11.615, 24.055),
    },
    Sprite {
        start: 358690,
        len: 560,
        anchor: (11.565, 24.075),
    },
    Sprite {
        start: 359250,
        len: 548,
        anchor: (11.875, 23.92),
    },
    Sprite {
        start: 359798,
        len: 676,
        anchor: (13.905, 23.705),
    },
    Sprite {
        start: 360474,
        len: 672,
        anchor: (15.46, 23.53),
    },
    Sprite {
        start: 361146,
        len: 726,
        anchor: (15.625, 23.5),
    },
    Sprite {
        start: 361872,
        len: 738,
        anchor: (14.475, 23.635),
    },
    Sprite {
        start: 362610,
        len: 568,
        anchor: (12.465, 23.875),
    },
    Sprite {
        start: 363178,
        len: 520,
        anchor: (11.985, 24.055),
    },
    Sprite {
        start: 363698,
        len: 500,
        anchor: (11.65, 24.075),
    },
    Sprite {
        start: 364198,
        len: 556,
        anchor: (12.485, 23.92),
    },
    Sprite {
        start: 364754,
        len: 714,
        anchor: (13.905, 23.705),
    },
    Sprite {
        start: 365468,
        len: 1378,
        anchor: (15.46, 23.53),
    },
    Sprite {
        start: 366846,
        len: 1432,
        anchor: (15.625, 23.5),
    },
    Sprite {
        start: 368278,
        len: 1390,
        anchor: (14.475, 23.635),
    },
    Sprite {
        start: 369668,
        len: 1156,
        anchor: (11.98, 23.875),
    },
    Sprite {
        start: 370824,
        len: 900,
        anchor: (11.615, 24.055),
    },
    Sprite {
        start: 371724,
        len: 794,
        anchor: (11.565, 24.075),
    },
    Sprite {
        start: 372518,
        len: 1054,
        anchor: (11.875, 23.92),
    },
    Sprite {
        start: 373572,
        len: 1416,
        anchor: (13.905, 23.705),
    },
    Sprite {
        start: 374988,
        len: 1406,
        anchor: (15.46, 23.53),
    },
    Sprite {
        start: 376394,
        len: 1388,
        anchor: (15.625, 23.5),
    },
    Sprite {
        start: 377782,
        len: 1422,
        anchor: (14.475, 23.635),
    },
    Sprite {
        start: 379204,
        len: 1154,
        anchor: (12.465, 23.875),
    },
    Sprite {
        start: 380358,
        len: 954,
        anchor: (11.985, 24.055),
    },
    Sprite {
        start: 381312,
        len: 834,
        anchor: (11.65, 24.075),
    },
    Sprite {
        start: 382146,
        len: 1066,
        anchor: (12.485, 23.92),
    },
    Sprite {
        start: 383212,
        len: 1378,
        anchor: (13.905, 23.705),
    },
    Sprite {
        start: 384590,
        len: 804,
        anchor: (12.675, 29.32),
    },
    Sprite {
        start: 385394,
        len: 710,
        anchor: (12.71, 29.33),
    },
    Sprite {
        start: 386104,
        len: 638,
        anchor: (12.435, 29.26),
    },
    Sprite {
        start: 386742,
        len: 642,
        anchor: (11.98, 29.5),
    },
    Sprite {
        start: 387384,
        len: 710,
        anchor: (11.615, 29.645),
    },
    Sprite {
        start: 388094,
        len: 616,
        anchor: (11.565, 29.66),
    },
    Sprite {
        start: 388710,
        len: 630,
        anchor: (11.875, 29.55),
    },
    Sprite {
        start: 389340,
        len: 660,
        anchor: (12.335, 29.32),
    },
    Sprite {
        start: 390000,
        len: 792,
        anchor: (12.675, 29.32),
    },
    Sprite {
        start: 390792,
        len: 710,
        anchor: (12.71, 29.33),
    },
    Sprite {
        start: 391502,
        len: 680,
        anchor: (12.435, 29.26),
    },
    Sprite {
        start: 392182,
        len: 586,
        anchor: (12.465, 29.5),
    },
    Sprite {
        start: 392768,
        len: 616,
        anchor: (11.985, 29.645),
    },
    Sprite {
        start: 393384,
        len: 734,
        anchor: (11.65, 29.66),
    },
    Sprite {
        start: 394118,
        len: 646,
        anchor: (12.485, 29.55),
    },
    Sprite {
        start: 394764,
        len: 632,
        anchor: (12.335, 29.32),
    },
    Sprite {
        start: 395396,
        len: 580,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 395976,
        len: 496,
        anchor: (12.71, 23.16),
    },
    Sprite {
        start: 396472,
        len: 422,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 396894,
        len: 346,
        anchor: (11.98, 23.205),
    },
    Sprite {
        start: 397240,
        len: 368,
        anchor: (11.615, 23.22),
    },
    Sprite {
        start: 397608,
        len: 298,
        anchor: (11.565, 23.225),
    },
    Sprite {
        start: 397906,
        len: 94,
        anchor: (11.875, 23.21),
    },
    Sprite {
        start: 398000,
        len: 94,
        anchor: (12.335, 23.185),
    },
    Sprite {
        start: 398094,
        len: 326,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 398420,
        len: 362,
        anchor: (12.71, 23.16),
    },
    Sprite {
        start: 398782,
        len: 400,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 399182,
        len: 380,
        anchor: (12.465, 23.205),
    },
    Sprite {
        start: 399562,
        len: 420,
        anchor: (11.985, 23.22),
    },
    Sprite {
        start: 399982,
        len: 500,
        anchor: (11.65, 23.225),
    },
    Sprite {
        start: 400482,
        len: 532,
        anchor: (12.485, 23.21),
    },
    Sprite {
        start: 401014,
        len: 492,
        anchor: (12.335, 23.185),
    },
    Sprite {
        start: 401506,
        len: 1096,
        anchor: (12.85, 23.165),
    },
    Sprite {
        start: 402602,
        len: 1122,
        anchor: (13.02, 23.16),
    },
    Sprite {
        start: 403724,
        len: 1030,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 404754,
        len: 916,
        anchor: (11.98, 23.205),
    },
    Sprite {
        start: 405670,
        len: 716,
        anchor: (11.615, 23.22),
    },
    Sprite {
        start: 406386,
        len: 626,
        anchor: (11.565, 23.225),
    },
    Sprite {
        start: 407012,
        len: 478,
        anchor: (11.875, 23.21),
    },
    Sprite {
        start: 407490,
        len: 380,
        anchor: (12.335, 23.185),
    },
    Sprite {
        start: 407870,
        len: 288,
        anchor: (12.775, 23.165),
    },
    Sprite {
        start: 408158,
        len: 280,
        anchor: (13.07, 23.16),
    },
    Sprite {
        start: 408438,
        len: 242,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 408680,
        len: 438,
        anchor: (13.6, 23.205),
    },
    Sprite {
        start: 409118,
        len: 472,
        anchor: (12.99, 23.22),
    },
    Sprite {
        start: 409590,
        len: 616,
        anchor: (13.025, 23.225),
    },
    Sprite {
        start: 410206,
        len: 882,
        anchor: (13.63, 23.21),
    },
    Sprite {
        start: 411088,
        len: 976,
        anchor: (12.76, 23.185),
    },
    Sprite {
        start: 412064,
        len: 1082,
        anchor: (12.85, 23.165),
    },
    Sprite {
        start: 413146,
        len: 1132,
        anchor: (13.02, 23.16),
    },
    Sprite {
        start: 414278,
        len: 1052,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 415330,
        len: 906,
        anchor: (11.98, 23.205),
    },
    Sprite {
        start: 416236,
        len: 718,
        anchor: (11.615, 23.22),
    },
    Sprite {
        start: 416954,
        len: 594,
        anchor: (11.565, 23.225),
    },
    Sprite {
        start: 417548,
        len: 470,
        anchor: (11.875, 23.21),
    },
    Sprite {
        start: 418018,
        len: 380,
        anchor: (12.335, 23.185),
    },
    Sprite {
        start: 418398,
        len: 288,
        anchor: (12.775, 23.165),
    },
    Sprite {
        start: 418686,
        len: 280,
        anchor: (13.07, 23.16),
    },
    Sprite {
        start: 418966,
        len: 242,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 419208,
        len: 448,
        anchor: (13.6, 23.205),
    },
    Sprite {
        start: 419656,
        len: 472,
        anchor: (12.99, 23.22),
    },
    Sprite {
        start: 420128,
        len: 642,
        anchor: (13.025, 23.225),
    },
    Sprite {
        start: 420770,
        len: 854,
        anchor: (13.63, 23.21),
    },
    Sprite {
        start: 421624,
        len: 972,
        anchor: (12.76, 23.185),
    },
    Sprite {
        start: 422596,
        len: 508,
        anchor: (12.85, 23.165),
    },
    Sprite {
        start: 423104,
        len: 558,
        anchor: (13.02, 23.16),
    },
    Sprite {
        start: 423662,
        len: 580,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 424242,
        len: 566,
        anchor: (11.98, 23.205),
    },
    Sprite {
        start: 424808,
        len: 388,
        anchor: (11.615, 23.22),
    },
    Sprite {
        start: 425196,
        len: 384,
        anchor: (11.565, 23.225),
    },
    Sprite {
        start: 425580,
        len: 248,
        anchor: (11.875, 23.21),
    },
    Sprite {
        start: 425828,
        len: 94,
        anchor: (12.335, 23.185),
    },
    Sprite {
        start: 425922,
        len: 94,
        anchor: (12.775, 23.165),
    },
    Sprite {
        start: 426016,
        len: 94,
        anchor: (13.07, 23.16),
    },
    Sprite {
        start: 426110,
        len: 94,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 426204,
        len: 200,
        anchor: (13.6, 23.205),
    },
    Sprite {
        start: 426404,
        len: 128,
        anchor: (12.99, 23.22),
    },
    Sprite {
        start: 426532,
        len: 372,
        anchor: (13.025, 23.225),
    },
    Sprite {
        start: 426904,
        len: 444,
        anchor: (13.63, 23.21),
    },
    Sprite {
        start: 427348,
        len: 536,
        anchor: (12.76, 23.185),
    },
    Sprite {
        start: 427884,
        len: 442,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 428326,
        len: 448,
        anchor: (12.71, 23.16),
    },
    Sprite {
        start: 428774,
        len: 468,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 429242,
        len: 468,
        anchor: (11.98, 23.205),
    },
    Sprite {
        start: 429710,
        len: 370,
        anchor: (11.615, 23.22),
    },
    Sprite {
        start: 430080,
        len: 300,
        anchor: (11.565, 23.225),
    },
    Sprite {
        start: 430380,
        len: 280,
        anchor: (11.875, 23.21),
    },
    Sprite {
        start: 430660,
        len: 94,
        anchor: (12.335, 23.185),
    },
    Sprite {
        start: 430754,
        len: 94,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 430848,
        len: 94,
        anchor: (12.71, 23.16),
    },
    Sprite {
        start: 430942,
        len: 94,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 431036,
        len: 186,
        anchor: (12.465, 23.205),
    },
    Sprite {
        start: 431222,
        len: 272,
        anchor: (12.7, 23.22),
    },
    Sprite {
        start: 431494,
        len: 354,
        anchor: (12.6, 23.225),
    },
    Sprite {
        start: 431848,
        len: 412,
        anchor: (12.485, 23.21),
    },
    Sprite {
        start: 432260,
        len: 466,
        anchor: (12.335, 23.185),
    },
    Sprite {
        start: 432726,
        len: 964,
        anchor: (12.92, 23.165),
    },
    Sprite {
        start: 433690,
        len: 954,
        anchor: (12.96, 23.16),
    },
    Sprite {
        start: 434644,
        len: 930,
        anchor: (12.7, 23.18),
    },
    Sprite {
        start: 435574,
        len: 952,
        anchor: (12.265, 23.205),
    },
    Sprite {
        start: 436526,
        len: 914,
        anchor: (11.92, 23.22),
    },
    Sprite {
        start: 437440,
        len: 902,
        anchor: (11.87, 23.225),
    },
    Sprite {
        start: 438342,
        len: 930,
        anchor: (12.165, 23.21),
    },
    Sprite {
        start: 439272,
        len: 860,
        anchor: (12.6, 23.185),
    },
    Sprite {
        start: 440132,
        len: 848,
        anchor: (12.92, 23.165),
    },
    Sprite {
        start: 440980,
        len: 874,
        anchor: (12.96, 23.16),
    },
    Sprite {
        start: 441854,
        len: 884,
        anchor: (12.7, 23.18),
    },
    Sprite {
        start: 442738,
        len: 940,
        anchor: (12.465, 23.205),
    },
    Sprite {
        start: 443678,
        len: 998,
        anchor: (12.265, 23.22),
    },
    Sprite {
        start: 444676,
        len: 984,
        anchor: (11.87, 23.225),
    },
    Sprite {
        start: 445660,
        len: 1062,
        anchor: (12.485, 23.21),
    },
    Sprite {
        start: 446722,
        len: 962,
        anchor: (12.6, 23.185),
    },
    Sprite {
        start: 447684,
        len: 340,
        anchor: (12.675, 26.38),
    },
    Sprite {
        start: 448024,
        len: 384,
        anchor: (12.71, 26.38),
    },
    Sprite {
        start: 448408,
        len: 310,
        anchor: (12.435, 26.38),
    },
    Sprite {
        start: 448718,
        len: 308,
        anchor: (11.98, 26.38),
    },
    Sprite {
        start: 449026,
        len: 314,
        anchor: (11.615, 26.38),
    },
    Sprite {
        start: 449340,
        len: 354,
        anchor: (11.565, 26.38),
    },
    Sprite {
        start: 449694,
        len: 362,
        anchor: (11.875, 26.38),
    },
    Sprite {
        start: 450056,
        len: 336,
        anchor: (12.335, 26.38),
    },
    Sprite {
        start: 450392,
        len: 292,
        anchor: (12.675, 26.38),
    },
    Sprite {
        start: 450684,
        len: 302,
        anchor: (12.71, 26.38),
    },
    Sprite {
        start: 450986,
        len: 308,
        anchor: (12.435, 26.38),
    },
    Sprite {
        start: 451294,
        len: 316,
        anchor: (12.465, 26.38),
    },
    Sprite {
        start: 451610,
        len: 346,
        anchor: (11.985, 26.38),
    },
    Sprite {
        start: 451956,
        len: 304,
        anchor: (11.65, 26.38),
    },
    Sprite {
        start: 452260,
        len: 328,
        anchor: (12.485, 26.38),
    },
    Sprite {
        start: 452588,
        len: 328,
        anchor: (12.335, 26.38),
    },
    Sprite {
        start: 452916,
        len: 498,
        anchor: (12.675, 26.715),
    },
    Sprite {
        start: 453414,
        len: 520,
        anchor: (12.71, 26.65),
    },
    Sprite {
        start: 453934,
        len: 516,
        anchor: (12.435, 26.915),
    },
    Sprite {
        start: 454450,
        len: 460,
        anchor: (11.98, 27.145),
    },
    Sprite {
        start: 454910,
        len: 408,
        anchor: (11.615, 27.285),
    },
    Sprite {
        start: 455318,
        len: 402,
        anchor: (11.565, 27.3),
    },
    Sprite {
        start: 455720,
        len: 434,
        anchor: (11.875, 27.19),
    },
    Sprite {
        start: 456154,
        len: 504,
        anchor: (12.335, 26.975),
    },
    Sprite {
        start: 456658,
        len: 514,
        anchor: (12.675, 26.715),
    },
    Sprite {
        start: 457172,
        len: 514,
        anchor: (12.71, 26.65),
    },
    Sprite {
        start: 457686,
        len: 524,
        anchor: (12.435, 26.915),
    },
    Sprite {
        start: 458210,
        len: 446,
        anchor: (12.465, 27.145),
    },
    Sprite {
        start: 458656,
        len: 410,
        anchor: (11.985, 27.285),
    },
    Sprite {
        start: 459066,
        len: 410,
        anchor: (11.65, 27.3),
    },
    Sprite {
        start: 459476,
        len: 438,
        anchor: (12.485, 27.19),
    },
    Sprite {
        start: 459914,
        len: 510,
        anchor: (12.335, 26.975),
    },
    Sprite {
        start: 460424,
        len: 292,
        anchor: (12.675, 28.935),
    },
    Sprite {
        start: 460716,
        len: 290,
        anchor: (12.71, 29.145),
    },
    Sprite {
        start: 461006,
        len: 272,
        anchor: (12.435, 29.33),
    },
    Sprite {
        start: 461278,
        len: 260,
        anchor: (11.98, 29.47),
    },
    Sprite {
        start: 461538,
        len: 274,
        anchor: (11.615, 29.535),
    },
    Sprite {
        start: 461812,
        len: 254,
        anchor: (11.565, 29.52),
    },
    Sprite {
        start: 462066,
        len: 252,
        anchor: (11.875, 29.425),
    },
    Sprite {
        start: 462318,
        len: 268,
        anchor: (12.335, 29.265),
    },
    Sprite {
        start: 462586,
        len: 246,
        anchor: (12.675, 29.065),
    },
    Sprite {
        start: 462832,
        len: 242,
        anchor: (12.71, 28.855),
    },
    Sprite {
        start: 463074,
        len: 272,
        anchor: (12.435, 28.665),
    },
    Sprite {
        start: 463346,
        len: 266,
        anchor: (12.465, 28.53),
    },
    Sprite {
        start: 463612,
        len: 250,
        anchor: (11.985, 28.465),
    },
    Sprite {
        start: 463862,
        len: 236,
        anchor: (11.65, 28.48),
    },
    Sprite {
        start: 464098,
        len: 256,
        anchor: (12.485, 28.575),
    },
    Sprite {
        start: 464354,
        len: 278,
        anchor: (12.335, 28.735),
    },
    Sprite {
        start: 464632,
        len: 318,
        anchor: (12.675, 28.935),
    },
    Sprite {
        start: 464950,
        len: 344,
        anchor: (12.71, 29.145),
    },
    Sprite {
        start: 465294,
        len: 374,
        anchor: (12.435, 29.33),
    },
    Sprite {
        start: 465668,
        len: 302,
        anchor: (11.98, 29.47),
    },
    Sprite {
        start: 465970,
        len: 346,
        anchor: (11.615, 29.535),
    },
    Sprite {
        start: 466316,
        len: 236,
        anchor: (11.565, 29.52),
    },
    Sprite {
        start: 466552,
        len: 278,
        anchor: (11.875, 29.425),
    },
    Sprite {
        start: 466830,
        len: 300,
        anchor: (12.335, 29.265),
    },
    Sprite {
        start: 467130,
        len: 336,
        anchor: (12.675, 29.065),
    },
    Sprite {
        start: 467466,
        len: 346,
        anchor: (12.71, 28.855),
    },
    Sprite {
        start: 467812,
        len: 292,
        anchor: (12.435, 28.665),
    },
    Sprite {
        start: 468104,
        len: 304,
        anchor: (12.465, 28.53),
    },
    Sprite {
        start: 468408,
        len: 266,
        anchor: (11.985, 28.465),
    },
    Sprite {
        start: 468674,
        len: 260,
        anchor: (11.65, 28.48),
    },
    Sprite {
        start: 468934,
        len: 294,
        anchor: (12.485, 28.575),
    },
    Sprite {
        start: 469228,
        len: 318,
        anchor: (12.335, 28.735),
    },
    Sprite {
        start: 469546,
        len: 706,
        anchor: (5.485, 7.68),
    },
    Sprite {
        start: 470252,
        len: 734,
        anchor: (5.485, 7.68),
    },
    Sprite {
        start: 470986,
        len: 744,
        anchor: (5.485, 7.68),
    },
    Sprite {
        start: 471730,
        len: 762,
        anchor: (5.485, 7.68),
    },
    Sprite {
        start: 472492,
        len: 790,
        anchor: (5.485, 7.68),
    },
    Sprite {
        start: 473282,
        len: 402,
        anchor: (3.665, 4.61),
    },
    Sprite {
        start: 473684,
        len: 416,
        anchor: (3.665, 4.61),
    },
    Sprite {
        start: 474100,
        len: 416,
        anchor: (3.665, 4.61),
    },
    Sprite {
        start: 474516,
        len: 430,
        anchor: (3.665, 4.61),
    },
    Sprite {
        start: 474946,
        len: 434,
        anchor: (3.665, 4.61),
    },
    Sprite {
        start: 475380,
        len: 172,
        anchor: (1.44, 1.44),
    },
    Sprite {
        start: 475552,
        len: 252,
        anchor: (2.16, 2.16),
    },
    Sprite {
        start: 475804,
        len: 214,
        anchor: (1.56, 1.895),
    },
    Sprite {
        start: 476018,
        len: 406,
        anchor: (1.955, 5.54),
    },
    Sprite {
        start: 476424,
        len: 296,
        anchor: (2.64, 2.415),
    },
    Sprite {
        start: 476720,
        len: 240,
        anchor: (1.8, 1.8),
    },
    Sprite {
        start: 476960,
        len: 230,
        anchor: (1.8, 1.8),
    },
    Sprite {
        start: 477190,
        len: 382,
        anchor: (1.8, 4.475),
    },
    Sprite {
        start: 477572,
        len: 342,
        anchor: (2.875, 2.955),
    },
    Sprite {
        start: 477914,
        len: 350,
        anchor: (2.95, 2.77),
    },
    Sprite {
        start: 478264,
        len: 364,
        anchor: (2.88, 4.025),
    },
    Sprite {
        start: 478628,
        len: 364,
        anchor: (2.88, 4.025),
    },
    Sprite {
        start: 478992,
        len: 298,
        anchor: (1.145, 4.41),
    },
    Sprite {
        start: 479290,
        len: 340,
        anchor: (2.27, 2.81),
    },
    Sprite {
        start: 479630,
        len: 244,
        anchor: (1.7, 1.635),
    },
    Sprite {
        start: 479874,
        len: 252,
        anchor: (1.58, 1.665),
    },
    Sprite {
        start: 480126,
        len: 210,
        anchor: (1.29, 1.615),
    },
    Sprite {
        start: 480336,
        len: 196,
        anchor: (1.445, 1.69),
    },
    Sprite {
        start: 480532,
        len: 294,
        anchor: (2.415, 2.79),
    },
    Sprite {
        start: 480826,
        len: 320,
        anchor: (2.815, 4.325),
    },
    Sprite {
        start: 481146,
        len: 374,
        anchor: (3.21, 5.865),
    },
    Sprite {
        start: 481520,
        len: 418,
        anchor: (3.61, 7.405),
    },
    Sprite {
        start: 481938,
        len: 644,
        anchor: (4.015, 9.905),
    },
    Sprite {
        start: 482582,
        len: 1806,
        anchor: (17.48, 17.48),
    },
    Sprite {
        start: 484388,
        len: 1720,
        anchor: (17.48, 17.48),
    },
    Sprite {
        start: 486108,
        len: 1580,
        anchor: (17.48, 17.48),
    },
    Sprite {
        start: 487688,
        len: 1588,
        anchor: (17.48, 17.48),
    },
    Sprite {
        start: 489276,
        len: 1614,
        anchor: (17.48, 17.48),
    },
    Sprite {
        start: 490890,
        len: 1720,
        anchor: (17.48, 17.48),
    },
    Sprite {
        start: 492610,
        len: 1716,
        anchor: (17.48, 17.48),
    },
    Sprite {
        start: 494326,
        len: 3126,
        anchor: (24.145, 24.145),
    },
    Sprite {
        start: 497452,
        len: 3142,
        anchor: (24.145, 24.145),
    },
    Sprite {
        start: 500594,
        len: 2946,
        anchor: (24.145, 24.145),
    },
    Sprite {
        start: 503540,
        len: 2874,
        anchor: (24.145, 24.145),
    },
    Sprite {
        start: 506414,
        len: 3114,
        anchor: (24.145, 24.145),
    },
    Sprite {
        start: 509528,
        len: 3088,
        anchor: (24.145, 24.145),
    },
    Sprite {
        start: 512616,
        len: 3116,
        anchor: (24.145, 24.145),
    },
    Sprite {
        start: 515732,
        len: 632,
        anchor: (4.27, 4.27),
    },
    Sprite {
        start: 516364,
        len: 664,
        anchor: (4.27, 4.27),
    },
    Sprite {
        start: 517028,
        len: 678,
        anchor: (4.27, 4.27),
    },
    Sprite {
        start: 517706,
        len: 688,
        anchor: (4.27, 4.27),
    },
    Sprite {
        start: 518394,
        len: 658,
        anchor: (4.27, 4.27),
    },
    Sprite {
        start: 519052,
        len: 646,
        anchor: (4.27, 4.27),
    },
    Sprite {
        start: 519698,
        len: 676,
        anchor: (4.27, 4.27),
    },
    Sprite {
        start: 520374,
        len: 636,
        anchor: (4.27, 4.27),
    },
    Sprite {
        start: 521010,
        len: 282,
        anchor: (11.0, 3.5),
    },
];

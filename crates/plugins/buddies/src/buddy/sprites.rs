// 由 scripts/buddies_pack.py 生成，别手改：改 buddies_render.py 重渲再打包。

use super::{Sprite, Wearable};

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

/// 戴的东西，头顶、脸上、脖子各一张表：可换色的主料、固定色的点缀各是哪张。
pub(super) static HEADS: [Wearable; 15] = [
    Wearable {
        base: Some(HEAD_BEANIE_BASE),
        trim: Some(HEAD_BEANIE_TRIM),
        eyes: false,
    }, // beanie
    Wearable {
        base: Some(HEAD_CAP_BASE),
        trim: None,
        eyes: false,
    }, // cap
    Wearable {
        base: Some(HEAD_FEDORA_BASE),
        trim: Some(HEAD_FEDORA_TRIM),
        eyes: false,
    }, // fedora
    Wearable {
        base: Some(HEAD_BERET_BASE),
        trim: None,
        eyes: false,
    }, // beret
    Wearable {
        base: Some(HEAD_GOGGLES_BASE),
        trim: Some(HEAD_GOGGLES_TRIM),
        eyes: true,
    }, // goggles
    Wearable {
        base: Some(HEAD_STRAW_BASE),
        trim: Some(HEAD_STRAW_TRIM),
        eyes: false,
    }, // straw
    Wearable {
        base: Some(HEAD_BANDANA_BASE),
        trim: Some(HEAD_BANDANA_TRIM),
        eyes: false,
    }, // bandana
    Wearable {
        base: Some(HEAD_HELMET_BASE),
        trim: Some(HEAD_HELMET_TRIM),
        eyes: false,
    }, // helmet
    Wearable {
        base: None,
        trim: Some(HEAD_SLICK_TRIM),
        eyes: false,
    }, // slick
    Wearable {
        base: None,
        trim: Some(HEAD_SPRIG_TRIM),
        eyes: false,
    }, // sprig
    Wearable {
        base: Some(HEAD_FLOWER_BASE),
        trim: Some(HEAD_FLOWER_TRIM),
        eyes: false,
    }, // flower
    Wearable {
        base: Some(HEAD_HEADPHONES_BASE),
        trim: Some(HEAD_HEADPHONES_TRIM),
        eyes: false,
    }, // headphones
    Wearable {
        base: Some(HEAD_ANTENNA_BASE),
        trim: Some(HEAD_ANTENNA_TRIM),
        eyes: false,
    }, // antenna
    Wearable {
        base: None,
        trim: Some(HEAD_CHEF_TRIM),
        eyes: false,
    }, // chef
    Wearable {
        base: Some(HEAD_BOW_BASE),
        trim: None,
        eyes: false,
    }, // bow
];
pub(super) static FACES: [Wearable; 2] = [
    Wearable {
        base: Some(FACE_GLASSES_BASE),
        trim: None,
        eyes: true,
    }, // glasses
    Wearable {
        base: Some(FACE_SUNGLASSES_BASE),
        trim: Some(FACE_SUNGLASSES_TRIM),
        eyes: true,
    }, // sunglasses
];
pub(super) static NECKS: [Wearable; 2] = [
    Wearable {
        base: Some(NECK_BOW_TIE_BASE),
        trim: None,
        eyes: false,
    }, // bow-tie
    Wearable {
        base: Some(NECK_SCARF_BASE),
        trim: None,
        eyes: false,
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
pub(super) const HEAD_BEANIE_TRIM: usize = 352;
pub(super) const HEAD_CAP_BASE: usize = 368;
pub(super) const HEAD_FEDORA_BASE: usize = 384;
pub(super) const HEAD_FEDORA_TRIM: usize = 400;
pub(super) const HEAD_BERET_BASE: usize = 416;
pub(super) const HEAD_GOGGLES_BASE: usize = 432;
pub(super) const HEAD_GOGGLES_TRIM: usize = 448;
pub(super) const HEAD_STRAW_BASE: usize = 464;
pub(super) const HEAD_STRAW_TRIM: usize = 480;
pub(super) const HEAD_BANDANA_BASE: usize = 496;
pub(super) const HEAD_BANDANA_TRIM: usize = 512;
pub(super) const HEAD_HELMET_BASE: usize = 528;
pub(super) const HEAD_HELMET_TRIM: usize = 544;
pub(super) const HEAD_SLICK_TRIM: usize = 560;
pub(super) const HEAD_SPRIG_TRIM: usize = 576;
pub(super) const HEAD_FLOWER_BASE: usize = 592;
pub(super) const HEAD_FLOWER_TRIM: usize = 608;
pub(super) const HEAD_HEADPHONES_BASE: usize = 624;
pub(super) const HEAD_HEADPHONES_TRIM: usize = 640;
pub(super) const HEAD_ANTENNA_BASE: usize = 656;
pub(super) const HEAD_ANTENNA_TRIM: usize = 672;
pub(super) const HEAD_CHEF_TRIM: usize = 688;
pub(super) const HEAD_BOW_BASE: usize = 704;
pub(super) const FACE_GLASSES_BASE: usize = 720;
pub(super) const FACE_SUNGLASSES_BASE: usize = 736;
pub(super) const FACE_SUNGLASSES_TRIM: usize = 752;
pub(super) const NECK_BOW_TIE_BASE: usize = 768;
pub(super) const NECK_SCARF_BASE: usize = 784;
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
        len: 744,
        anchor: (11.22, 11.22),
    },
    Sprite {
        start: 58364,
        len: 812,
        anchor: (11.22, 11.22),
    },
    Sprite {
        start: 59176,
        len: 808,
        anchor: (11.22, 11.22),
    },
    Sprite {
        start: 59984,
        len: 998,
        anchor: (9.045, 9.045),
    },
    Sprite {
        start: 60982,
        len: 970,
        anchor: (9.045, 9.045),
    },
    Sprite {
        start: 61952,
        len: 910,
        anchor: (9.045, 9.045),
    },
    Sprite {
        start: 62862,
        len: 996,
        anchor: (9.045, 9.045),
    },
    Sprite {
        start: 63858,
        len: 964,
        anchor: (9.045, 9.045),
    },
    Sprite {
        start: 64822,
        len: 964,
        anchor: (9.045, 9.045),
    },
    Sprite {
        start: 65786,
        len: 1022,
        anchor: (9.045, 9.045),
    },
    Sprite {
        start: 66808,
        len: 990,
        anchor: (9.045, 9.045),
    },
    Sprite {
        start: 67798,
        len: 416,
        anchor: (11.675, 11.675),
    },
    Sprite {
        start: 68214,
        len: 492,
        anchor: (11.675, 11.675),
    },
    Sprite {
        start: 68706,
        len: 394,
        anchor: (11.675, 11.675),
    },
    Sprite {
        start: 69100,
        len: 544,
        anchor: (11.675, 11.675),
    },
    Sprite {
        start: 69644,
        len: 426,
        anchor: (11.675, 11.675),
    },
    Sprite {
        start: 70070,
        len: 576,
        anchor: (11.675, 11.675),
    },
    Sprite {
        start: 70646,
        len: 406,
        anchor: (11.675, 11.675),
    },
    Sprite {
        start: 71052,
        len: 494,
        anchor: (11.675, 11.675),
    },
    Sprite {
        start: 71546,
        len: 882,
        anchor: (17.465, 17.465),
    },
    Sprite {
        start: 72428,
        len: 1172,
        anchor: (17.465, 17.465),
    },
    Sprite {
        start: 73600,
        len: 828,
        anchor: (17.465, 17.465),
    },
    Sprite {
        start: 74428,
        len: 1170,
        anchor: (17.465, 17.465),
    },
    Sprite {
        start: 75598,
        len: 854,
        anchor: (17.465, 17.465),
    },
    Sprite {
        start: 76452,
        len: 1198,
        anchor: (17.465, 17.465),
    },
    Sprite {
        start: 77650,
        len: 806,
        anchor: (17.465, 17.465),
    },
    Sprite {
        start: 78456,
        len: 1144,
        anchor: (17.465, 17.465),
    },
    Sprite {
        start: 79600,
        len: 416,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 80016,
        len: 404,
        anchor: (12.71, 23.16),
    },
    Sprite {
        start: 80420,
        len: 398,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 80818,
        len: 380,
        anchor: (11.98, 23.205),
    },
    Sprite {
        start: 81198,
        len: 408,
        anchor: (11.615, 23.22),
    },
    Sprite {
        start: 81606,
        len: 308,
        anchor: (11.565, 23.225),
    },
    Sprite {
        start: 81914,
        len: 278,
        anchor: (11.875, 23.21),
    },
    Sprite {
        start: 82192,
        len: 94,
        anchor: (12.335, 23.185),
    },
    Sprite {
        start: 82286,
        len: 94,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 82380,
        len: 94,
        anchor: (12.71, 23.16),
    },
    Sprite {
        start: 82474,
        len: 94,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 82568,
        len: 94,
        anchor: (12.465, 23.205),
    },
    Sprite {
        start: 82662,
        len: 92,
        anchor: (11.985, 23.22),
    },
    Sprite {
        start: 82754,
        len: 258,
        anchor: (12.05, 23.225),
    },
    Sprite {
        start: 83012,
        len: 274,
        anchor: (12.485, 23.21),
    },
    Sprite {
        start: 83286,
        len: 342,
        anchor: (12.335, 23.185),
    },
    Sprite {
        start: 83628,
        len: 398,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 84026,
        len: 392,
        anchor: (12.71, 23.16),
    },
    Sprite {
        start: 84418,
        len: 386,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 84804,
        len: 366,
        anchor: (11.98, 23.205),
    },
    Sprite {
        start: 85170,
        len: 256,
        anchor: (11.615, 23.22),
    },
    Sprite {
        start: 85426,
        len: 92,
        anchor: (11.565, 23.225),
    },
    Sprite {
        start: 85518,
        len: 94,
        anchor: (11.875, 23.21),
    },
    Sprite {
        start: 85612,
        len: 94,
        anchor: (12.335, 23.185),
    },
    Sprite {
        start: 85706,
        len: 94,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 85800,
        len: 94,
        anchor: (12.71, 23.16),
    },
    Sprite {
        start: 85894,
        len: 94,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 85988,
        len: 232,
        anchor: (12.465, 23.205),
    },
    Sprite {
        start: 86220,
        len: 332,
        anchor: (12.085, 23.22),
    },
    Sprite {
        start: 86552,
        len: 378,
        anchor: (11.65, 23.225),
    },
    Sprite {
        start: 86930,
        len: 394,
        anchor: (12.485, 23.21),
    },
    Sprite {
        start: 87324,
        len: 418,
        anchor: (12.335, 23.185),
    },
    Sprite {
        start: 87742,
        len: 322,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 88064,
        len: 294,
        anchor: (12.71, 23.16),
    },
    Sprite {
        start: 88358,
        len: 324,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 88682,
        len: 298,
        anchor: (11.98, 23.205),
    },
    Sprite {
        start: 88980,
        len: 268,
        anchor: (11.615, 23.22),
    },
    Sprite {
        start: 89248,
        len: 238,
        anchor: (11.565, 23.225),
    },
    Sprite {
        start: 89486,
        len: 290,
        anchor: (11.875, 23.21),
    },
    Sprite {
        start: 89776,
        len: 94,
        anchor: (12.335, 23.185),
    },
    Sprite {
        start: 89870,
        len: 94,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 89964,
        len: 94,
        anchor: (12.71, 23.16),
    },
    Sprite {
        start: 90058,
        len: 94,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 90152,
        len: 94,
        anchor: (12.465, 23.205),
    },
    Sprite {
        start: 90246,
        len: 92,
        anchor: (11.985, 23.22),
    },
    Sprite {
        start: 90338,
        len: 216,
        anchor: (12.165, 23.225),
    },
    Sprite {
        start: 90554,
        len: 224,
        anchor: (12.485, 23.21),
    },
    Sprite {
        start: 90778,
        len: 284,
        anchor: (12.335, 23.185),
    },
    Sprite {
        start: 91062,
        len: 320,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 91382,
        len: 330,
        anchor: (12.71, 23.16),
    },
    Sprite {
        start: 91712,
        len: 270,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 91982,
        len: 248,
        anchor: (11.98, 23.205),
    },
    Sprite {
        start: 92230,
        len: 224,
        anchor: (11.615, 23.22),
    },
    Sprite {
        start: 92454,
        len: 92,
        anchor: (11.565, 23.225),
    },
    Sprite {
        start: 92546,
        len: 94,
        anchor: (11.875, 23.21),
    },
    Sprite {
        start: 92640,
        len: 94,
        anchor: (12.335, 23.185),
    },
    Sprite {
        start: 92734,
        len: 94,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 92828,
        len: 94,
        anchor: (12.71, 23.16),
    },
    Sprite {
        start: 92922,
        len: 94,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 93016,
        len: 246,
        anchor: (12.465, 23.205),
    },
    Sprite {
        start: 93262,
        len: 238,
        anchor: (12.22, 23.22),
    },
    Sprite {
        start: 93500,
        len: 288,
        anchor: (11.65, 23.225),
    },
    Sprite {
        start: 93788,
        len: 306,
        anchor: (12.485, 23.21),
    },
    Sprite {
        start: 94094,
        len: 344,
        anchor: (12.335, 23.185),
    },
    Sprite {
        start: 94438,
        len: 218,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 94656,
        len: 270,
        anchor: (12.71, 23.16),
    },
    Sprite {
        start: 94926,
        len: 238,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 95164,
        len: 254,
        anchor: (11.98, 23.205),
    },
    Sprite {
        start: 95418,
        len: 268,
        anchor: (11.615, 23.22),
    },
    Sprite {
        start: 95686,
        len: 216,
        anchor: (11.565, 23.225),
    },
    Sprite {
        start: 95902,
        len: 232,
        anchor: (11.875, 23.21),
    },
    Sprite {
        start: 96134,
        len: 94,
        anchor: (12.335, 23.185),
    },
    Sprite {
        start: 96228,
        len: 94,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 96322,
        len: 94,
        anchor: (12.71, 23.16),
    },
    Sprite {
        start: 96416,
        len: 94,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 96510,
        len: 94,
        anchor: (12.465, 23.205),
    },
    Sprite {
        start: 96604,
        len: 92,
        anchor: (11.985, 23.22),
    },
    Sprite {
        start: 96696,
        len: 206,
        anchor: (12.095, 23.225),
    },
    Sprite {
        start: 96902,
        len: 214,
        anchor: (12.485, 23.21),
    },
    Sprite {
        start: 97116,
        len: 262,
        anchor: (12.335, 23.185),
    },
    Sprite {
        start: 97378,
        len: 240,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 97618,
        len: 238,
        anchor: (12.71, 23.16),
    },
    Sprite {
        start: 97856,
        len: 232,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 98088,
        len: 242,
        anchor: (11.98, 23.205),
    },
    Sprite {
        start: 98330,
        len: 184,
        anchor: (11.615, 23.22),
    },
    Sprite {
        start: 98514,
        len: 92,
        anchor: (11.565, 23.225),
    },
    Sprite {
        start: 98606,
        len: 94,
        anchor: (11.875, 23.21),
    },
    Sprite {
        start: 98700,
        len: 94,
        anchor: (12.335, 23.185),
    },
    Sprite {
        start: 98794,
        len: 94,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 98888,
        len: 94,
        anchor: (12.71, 23.16),
    },
    Sprite {
        start: 98982,
        len: 94,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 99076,
        len: 184,
        anchor: (12.465, 23.205),
    },
    Sprite {
        start: 99260,
        len: 200,
        anchor: (12.16, 23.22),
    },
    Sprite {
        start: 99460,
        len: 244,
        anchor: (11.65, 23.225),
    },
    Sprite {
        start: 99704,
        len: 222,
        anchor: (12.485, 23.21),
    },
    Sprite {
        start: 99926,
        len: 264,
        anchor: (12.335, 23.185),
    },
    Sprite {
        start: 100190,
        len: 328,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 100518,
        len: 388,
        anchor: (12.71, 23.16),
    },
    Sprite {
        start: 100906,
        len: 352,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 101258,
        len: 326,
        anchor: (11.98, 23.205),
    },
    Sprite {
        start: 101584,
        len: 324,
        anchor: (11.615, 23.22),
    },
    Sprite {
        start: 101908,
        len: 272,
        anchor: (11.565, 23.225),
    },
    Sprite {
        start: 102180,
        len: 270,
        anchor: (11.875, 23.21),
    },
    Sprite {
        start: 102450,
        len: 94,
        anchor: (12.335, 23.185),
    },
    Sprite {
        start: 102544,
        len: 94,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 102638,
        len: 94,
        anchor: (12.71, 23.16),
    },
    Sprite {
        start: 102732,
        len: 94,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 102826,
        len: 94,
        anchor: (12.465, 23.205),
    },
    Sprite {
        start: 102920,
        len: 92,
        anchor: (11.985, 23.22),
    },
    Sprite {
        start: 103012,
        len: 226,
        anchor: (12.135, 23.225),
    },
    Sprite {
        start: 103238,
        len: 274,
        anchor: (12.485, 23.21),
    },
    Sprite {
        start: 103512,
        len: 302,
        anchor: (12.335, 23.185),
    },
    Sprite {
        start: 103814,
        len: 332,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 104146,
        len: 384,
        anchor: (12.71, 23.16),
    },
    Sprite {
        start: 104530,
        len: 320,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 104850,
        len: 292,
        anchor: (11.98, 23.205),
    },
    Sprite {
        start: 105142,
        len: 218,
        anchor: (11.615, 23.22),
    },
    Sprite {
        start: 105360,
        len: 110,
        anchor: (11.565, 23.225),
    },
    Sprite {
        start: 105470,
        len: 94,
        anchor: (11.875, 23.21),
    },
    Sprite {
        start: 105564,
        len: 94,
        anchor: (12.335, 23.185),
    },
    Sprite {
        start: 105658,
        len: 94,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 105752,
        len: 94,
        anchor: (12.71, 23.16),
    },
    Sprite {
        start: 105846,
        len: 94,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 105940,
        len: 200,
        anchor: (12.465, 23.205),
    },
    Sprite {
        start: 106140,
        len: 242,
        anchor: (12.18, 23.22),
    },
    Sprite {
        start: 106382,
        len: 288,
        anchor: (11.65, 23.225),
    },
    Sprite {
        start: 106670,
        len: 320,
        anchor: (12.485, 23.21),
    },
    Sprite {
        start: 106990,
        len: 328,
        anchor: (12.335, 23.185),
    },
    Sprite {
        start: 107318,
        len: 466,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 107784,
        len: 506,
        anchor: (12.71, 23.16),
    },
    Sprite {
        start: 108290,
        len: 444,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 108734,
        len: 422,
        anchor: (11.98, 23.205),
    },
    Sprite {
        start: 109156,
        len: 388,
        anchor: (11.615, 23.22),
    },
    Sprite {
        start: 109544,
        len: 316,
        anchor: (11.565, 23.225),
    },
    Sprite {
        start: 109860,
        len: 300,
        anchor: (11.875, 23.21),
    },
    Sprite {
        start: 110160,
        len: 94,
        anchor: (12.335, 23.185),
    },
    Sprite {
        start: 110254,
        len: 94,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 110348,
        len: 94,
        anchor: (12.71, 23.16),
    },
    Sprite {
        start: 110442,
        len: 94,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 110536,
        len: 94,
        anchor: (12.465, 23.205),
    },
    Sprite {
        start: 110630,
        len: 92,
        anchor: (11.985, 23.22),
    },
    Sprite {
        start: 110722,
        len: 266,
        anchor: (12.105, 23.225),
    },
    Sprite {
        start: 110988,
        len: 308,
        anchor: (12.485, 23.21),
    },
    Sprite {
        start: 111296,
        len: 388,
        anchor: (12.335, 23.185),
    },
    Sprite {
        start: 111684,
        len: 460,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 112144,
        len: 446,
        anchor: (12.71, 23.16),
    },
    Sprite {
        start: 112590,
        len: 424,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 113014,
        len: 346,
        anchor: (11.98, 23.205),
    },
    Sprite {
        start: 113360,
        len: 262,
        anchor: (11.615, 23.22),
    },
    Sprite {
        start: 113622,
        len: 92,
        anchor: (11.565, 23.225),
    },
    Sprite {
        start: 113714,
        len: 94,
        anchor: (11.875, 23.21),
    },
    Sprite {
        start: 113808,
        len: 94,
        anchor: (12.335, 23.185),
    },
    Sprite {
        start: 113902,
        len: 94,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 113996,
        len: 94,
        anchor: (12.71, 23.16),
    },
    Sprite {
        start: 114090,
        len: 94,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 114184,
        len: 264,
        anchor: (12.465, 23.205),
    },
    Sprite {
        start: 114448,
        len: 296,
        anchor: (12.1, 23.22),
    },
    Sprite {
        start: 114744,
        len: 356,
        anchor: (11.65, 23.225),
    },
    Sprite {
        start: 115100,
        len: 462,
        anchor: (12.485, 23.21),
    },
    Sprite {
        start: 115562,
        len: 506,
        anchor: (12.335, 23.185),
    },
    Sprite {
        start: 116068,
        len: 172,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 116240,
        len: 180,
        anchor: (12.71, 23.16),
    },
    Sprite {
        start: 116420,
        len: 190,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 116610,
        len: 164,
        anchor: (11.98, 23.205),
    },
    Sprite {
        start: 116774,
        len: 160,
        anchor: (11.615, 23.22),
    },
    Sprite {
        start: 116934,
        len: 148,
        anchor: (11.565, 23.225),
    },
    Sprite {
        start: 117082,
        len: 94,
        anchor: (11.875, 23.21),
    },
    Sprite {
        start: 117176,
        len: 94,
        anchor: (12.335, 23.185),
    },
    Sprite {
        start: 117270,
        len: 94,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 117364,
        len: 94,
        anchor: (12.71, 23.16),
    },
    Sprite {
        start: 117458,
        len: 94,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 117552,
        len: 94,
        anchor: (12.465, 23.205),
    },
    Sprite {
        start: 117646,
        len: 158,
        anchor: (11.985, 23.22),
    },
    Sprite {
        start: 117804,
        len: 216,
        anchor: (11.84, 23.225),
    },
    Sprite {
        start: 118020,
        len: 186,
        anchor: (12.485, 23.21),
    },
    Sprite {
        start: 118206,
        len: 176,
        anchor: (12.335, 23.185),
    },
    Sprite {
        start: 118382,
        len: 202,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 118584,
        len: 168,
        anchor: (12.71, 23.16),
    },
    Sprite {
        start: 118752,
        len: 170,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 118922,
        len: 184,
        anchor: (11.98, 23.205),
    },
    Sprite {
        start: 119106,
        len: 200,
        anchor: (11.615, 23.22),
    },
    Sprite {
        start: 119306,
        len: 198,
        anchor: (11.565, 23.225),
    },
    Sprite {
        start: 119504,
        len: 94,
        anchor: (11.875, 23.21),
    },
    Sprite {
        start: 119598,
        len: 94,
        anchor: (12.335, 23.185),
    },
    Sprite {
        start: 119692,
        len: 94,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 119786,
        len: 94,
        anchor: (12.71, 23.16),
    },
    Sprite {
        start: 119880,
        len: 94,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 119974,
        len: 94,
        anchor: (12.465, 23.205),
    },
    Sprite {
        start: 120068,
        len: 136,
        anchor: (11.985, 23.22),
    },
    Sprite {
        start: 120204,
        len: 206,
        anchor: (11.835, 23.225),
    },
    Sprite {
        start: 120410,
        len: 188,
        anchor: (12.485, 23.21),
    },
    Sprite {
        start: 120598,
        len: 180,
        anchor: (12.335, 23.185),
    },
    Sprite {
        start: 120778,
        len: 260,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 121038,
        len: 254,
        anchor: (12.71, 23.16),
    },
    Sprite {
        start: 121292,
        len: 226,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 121518,
        len: 260,
        anchor: (11.98, 23.205),
    },
    Sprite {
        start: 121778,
        len: 232,
        anchor: (11.615, 23.22),
    },
    Sprite {
        start: 122010,
        len: 108,
        anchor: (11.565, 23.225),
    },
    Sprite {
        start: 122118,
        len: 94,
        anchor: (11.875, 23.21),
    },
    Sprite {
        start: 122212,
        len: 94,
        anchor: (12.335, 23.185),
    },
    Sprite {
        start: 122306,
        len: 94,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 122400,
        len: 94,
        anchor: (12.71, 23.16),
    },
    Sprite {
        start: 122494,
        len: 94,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 122588,
        len: 94,
        anchor: (12.465, 23.205),
    },
    Sprite {
        start: 122682,
        len: 92,
        anchor: (11.985, 23.22),
    },
    Sprite {
        start: 122774,
        len: 224,
        anchor: (11.65, 23.225),
    },
    Sprite {
        start: 122998,
        len: 272,
        anchor: (12.485, 23.21),
    },
    Sprite {
        start: 123270,
        len: 212,
        anchor: (12.335, 23.185),
    },
    Sprite {
        start: 123482,
        len: 194,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 123676,
        len: 196,
        anchor: (12.71, 23.16),
    },
    Sprite {
        start: 123872,
        len: 210,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 124082,
        len: 204,
        anchor: (11.98, 23.205),
    },
    Sprite {
        start: 124286,
        len: 182,
        anchor: (11.615, 23.22),
    },
    Sprite {
        start: 124468,
        len: 110,
        anchor: (11.565, 23.225),
    },
    Sprite {
        start: 124578,
        len: 94,
        anchor: (11.875, 23.21),
    },
    Sprite {
        start: 124672,
        len: 94,
        anchor: (12.335, 23.185),
    },
    Sprite {
        start: 124766,
        len: 94,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 124860,
        len: 94,
        anchor: (12.71, 23.16),
    },
    Sprite {
        start: 124954,
        len: 94,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 125048,
        len: 94,
        anchor: (12.465, 23.205),
    },
    Sprite {
        start: 125142,
        len: 92,
        anchor: (11.985, 23.22),
    },
    Sprite {
        start: 125234,
        len: 212,
        anchor: (11.65, 23.225),
    },
    Sprite {
        start: 125446,
        len: 218,
        anchor: (12.485, 23.21),
    },
    Sprite {
        start: 125664,
        len: 192,
        anchor: (12.335, 23.185),
    },
    Sprite {
        start: 125856,
        len: 358,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 126214,
        len: 326,
        anchor: (12.71, 23.16),
    },
    Sprite {
        start: 126540,
        len: 338,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 126878,
        len: 336,
        anchor: (11.98, 23.205),
    },
    Sprite {
        start: 127214,
        len: 288,
        anchor: (11.615, 23.22),
    },
    Sprite {
        start: 127502,
        len: 182,
        anchor: (11.565, 23.225),
    },
    Sprite {
        start: 127684,
        len: 94,
        anchor: (11.875, 23.21),
    },
    Sprite {
        start: 127778,
        len: 94,
        anchor: (12.335, 23.185),
    },
    Sprite {
        start: 127872,
        len: 94,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 127966,
        len: 94,
        anchor: (12.71, 23.16),
    },
    Sprite {
        start: 128060,
        len: 94,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 128154,
        len: 94,
        anchor: (12.465, 23.205),
    },
    Sprite {
        start: 128248,
        len: 92,
        anchor: (11.985, 23.22),
    },
    Sprite {
        start: 128340,
        len: 244,
        anchor: (11.65, 23.225),
    },
    Sprite {
        start: 128584,
        len: 332,
        anchor: (12.485, 23.21),
    },
    Sprite {
        start: 128916,
        len: 312,
        anchor: (12.335, 23.185),
    },
    Sprite {
        start: 129228,
        len: 1224,
        anchor: (12.675, 26.38),
    },
    Sprite {
        start: 130452,
        len: 1212,
        anchor: (12.71, 26.38),
    },
    Sprite {
        start: 131664,
        len: 1186,
        anchor: (12.435, 26.38),
    },
    Sprite {
        start: 132850,
        len: 1166,
        anchor: (11.98, 26.38),
    },
    Sprite {
        start: 134016,
        len: 1018,
        anchor: (11.615, 26.38),
    },
    Sprite {
        start: 135034,
        len: 1038,
        anchor: (11.565, 26.38),
    },
    Sprite {
        start: 136072,
        len: 1100,
        anchor: (11.875, 26.38),
    },
    Sprite {
        start: 137172,
        len: 1132,
        anchor: (12.335, 26.38),
    },
    Sprite {
        start: 138304,
        len: 1138,
        anchor: (12.675, 26.38),
    },
    Sprite {
        start: 139442,
        len: 1196,
        anchor: (12.71, 26.38),
    },
    Sprite {
        start: 140638,
        len: 1170,
        anchor: (12.435, 26.38),
    },
    Sprite {
        start: 141808,
        len: 1100,
        anchor: (12.465, 26.38),
    },
    Sprite {
        start: 142908,
        len: 1120,
        anchor: (11.985, 26.38),
    },
    Sprite {
        start: 144028,
        len: 1134,
        anchor: (11.65, 26.38),
    },
    Sprite {
        start: 145162,
        len: 1080,
        anchor: (12.485, 26.38),
    },
    Sprite {
        start: 146242,
        len: 1130,
        anchor: (12.335, 26.38),
    },
    Sprite {
        start: 147372,
        len: 340,
        anchor: (12.675, 26.38),
    },
    Sprite {
        start: 147712,
        len: 384,
        anchor: (12.71, 26.38),
    },
    Sprite {
        start: 148096,
        len: 310,
        anchor: (12.435, 26.38),
    },
    Sprite {
        start: 148406,
        len: 308,
        anchor: (11.98, 26.38),
    },
    Sprite {
        start: 148714,
        len: 314,
        anchor: (11.615, 26.38),
    },
    Sprite {
        start: 149028,
        len: 354,
        anchor: (11.565, 26.38),
    },
    Sprite {
        start: 149382,
        len: 362,
        anchor: (11.875, 26.38),
    },
    Sprite {
        start: 149744,
        len: 336,
        anchor: (12.335, 26.38),
    },
    Sprite {
        start: 150080,
        len: 292,
        anchor: (12.675, 26.38),
    },
    Sprite {
        start: 150372,
        len: 302,
        anchor: (12.71, 26.38),
    },
    Sprite {
        start: 150674,
        len: 308,
        anchor: (12.435, 26.38),
    },
    Sprite {
        start: 150982,
        len: 316,
        anchor: (12.465, 26.38),
    },
    Sprite {
        start: 151298,
        len: 346,
        anchor: (11.985, 26.38),
    },
    Sprite {
        start: 151644,
        len: 304,
        anchor: (11.65, 26.38),
    },
    Sprite {
        start: 151948,
        len: 328,
        anchor: (12.485, 26.38),
    },
    Sprite {
        start: 152276,
        len: 328,
        anchor: (12.335, 26.38),
    },
    Sprite {
        start: 152604,
        len: 998,
        anchor: (12.675, 23.925),
    },
    Sprite {
        start: 153602,
        len: 964,
        anchor: (12.71, 23.925),
    },
    Sprite {
        start: 154566,
        len: 1040,
        anchor: (12.435, 23.925),
    },
    Sprite {
        start: 155606,
        len: 992,
        anchor: (11.98, 23.925),
    },
    Sprite {
        start: 156598,
        len: 1074,
        anchor: (11.615, 23.925),
    },
    Sprite {
        start: 157672,
        len: 1032,
        anchor: (11.565, 23.925),
    },
    Sprite {
        start: 158704,
        len: 1016,
        anchor: (11.875, 23.925),
    },
    Sprite {
        start: 159720,
        len: 990,
        anchor: (12.335, 23.925),
    },
    Sprite {
        start: 160710,
        len: 870,
        anchor: (12.675, 23.925),
    },
    Sprite {
        start: 161580,
        len: 896,
        anchor: (12.71, 23.925),
    },
    Sprite {
        start: 162476,
        len: 958,
        anchor: (12.435, 23.925),
    },
    Sprite {
        start: 163434,
        len: 1044,
        anchor: (14.47, 23.925),
    },
    Sprite {
        start: 164478,
        len: 954,
        anchor: (15.77, 23.925),
    },
    Sprite {
        start: 165432,
        len: 1062,
        anchor: (15.91, 23.925),
    },
    Sprite {
        start: 166494,
        len: 1060,
        anchor: (14.905, 23.925),
    },
    Sprite {
        start: 167554,
        len: 998,
        anchor: (12.62, 23.925),
    },
    Sprite {
        start: 168552,
        len: 1284,
        anchor: (12.675, 27.07),
    },
    Sprite {
        start: 169836,
        len: 1266,
        anchor: (12.71, 27.06),
    },
    Sprite {
        start: 171102,
        len: 1240,
        anchor: (12.435, 27.105),
    },
    Sprite {
        start: 172342,
        len: 1260,
        anchor: (11.98, 27.175),
    },
    Sprite {
        start: 173602,
        len: 1238,
        anchor: (11.615, 27.225),
    },
    Sprite {
        start: 174840,
        len: 1264,
        anchor: (11.565, 27.235),
    },
    Sprite {
        start: 176104,
        len: 1250,
        anchor: (11.875, 27.195),
    },
    Sprite {
        start: 177354,
        len: 1266,
        anchor: (12.335, 27.12),
    },
    Sprite {
        start: 178620,
        len: 1312,
        anchor: (12.675, 27.07),
    },
    Sprite {
        start: 179932,
        len: 1218,
        anchor: (12.71, 27.06),
    },
    Sprite {
        start: 181150,
        len: 1314,
        anchor: (12.435, 27.105),
    },
    Sprite {
        start: 182464,
        len: 1236,
        anchor: (12.465, 27.175),
    },
    Sprite {
        start: 183700,
        len: 1216,
        anchor: (11.985, 27.225),
    },
    Sprite {
        start: 184916,
        len: 1250,
        anchor: (11.65, 27.235),
    },
    Sprite {
        start: 186166,
        len: 1244,
        anchor: (12.485, 27.195),
    },
    Sprite {
        start: 187410,
        len: 1246,
        anchor: (12.335, 27.12),
    },
    Sprite {
        start: 188656,
        len: 578,
        anchor: (12.675, 27.07),
    },
    Sprite {
        start: 189234,
        len: 556,
        anchor: (12.71, 27.06),
    },
    Sprite {
        start: 189790,
        len: 544,
        anchor: (12.435, 27.105),
    },
    Sprite {
        start: 190334,
        len: 500,
        anchor: (11.98, 27.175),
    },
    Sprite {
        start: 190834,
        len: 534,
        anchor: (11.615, 27.225),
    },
    Sprite {
        start: 191368,
        len: 506,
        anchor: (11.565, 27.235),
    },
    Sprite {
        start: 191874,
        len: 530,
        anchor: (11.875, 27.195),
    },
    Sprite {
        start: 192404,
        len: 522,
        anchor: (12.335, 27.12),
    },
    Sprite {
        start: 192926,
        len: 590,
        anchor: (12.675, 27.07),
    },
    Sprite {
        start: 193516,
        len: 544,
        anchor: (12.71, 27.06),
    },
    Sprite {
        start: 194060,
        len: 528,
        anchor: (12.435, 27.105),
    },
    Sprite {
        start: 194588,
        len: 500,
        anchor: (12.465, 27.175),
    },
    Sprite {
        start: 195088,
        len: 510,
        anchor: (11.985, 27.225),
    },
    Sprite {
        start: 195598,
        len: 520,
        anchor: (11.65, 27.235),
    },
    Sprite {
        start: 196118,
        len: 468,
        anchor: (12.485, 27.195),
    },
    Sprite {
        start: 196586,
        len: 550,
        anchor: (12.335, 27.12),
    },
    Sprite {
        start: 197136,
        len: 906,
        anchor: (12.675, 26.335),
    },
    Sprite {
        start: 198042,
        len: 764,
        anchor: (12.71, 25.925),
    },
    Sprite {
        start: 198806,
        len: 804,
        anchor: (12.435, 25.475),
    },
    Sprite {
        start: 199610,
        len: 782,
        anchor: (11.98, 25.565),
    },
    Sprite {
        start: 200392,
        len: 746,
        anchor: (11.615, 25.56),
    },
    Sprite {
        start: 201138,
        len: 752,
        anchor: (11.565, 25.505),
    },
    Sprite {
        start: 201890,
        len: 778,
        anchor: (11.875, 25.435),
    },
    Sprite {
        start: 202668,
        len: 802,
        anchor: (12.335, 25.29),
    },
    Sprite {
        start: 203470,
        len: 758,
        anchor: (12.675, 25.56),
    },
    Sprite {
        start: 204228,
        len: 858,
        anchor: (12.71, 25.985),
    },
    Sprite {
        start: 205086,
        len: 864,
        anchor: (12.435, 26.375),
    },
    Sprite {
        start: 205950,
        len: 902,
        anchor: (12.465, 26.675),
    },
    Sprite {
        start: 206852,
        len: 836,
        anchor: (11.985, 26.88),
    },
    Sprite {
        start: 207688,
        len: 814,
        anchor: (11.65, 26.945),
    },
    Sprite {
        start: 208502,
        len: 926,
        anchor: (12.485, 26.87),
    },
    Sprite {
        start: 209428,
        len: 812,
        anchor: (12.335, 26.665),
    },
    Sprite {
        start: 210240,
        len: 830,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 211070,
        len: 788,
        anchor: (12.71, 23.16),
    },
    Sprite {
        start: 211858,
        len: 820,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 212678,
        len: 652,
        anchor: (11.98, 23.205),
    },
    Sprite {
        start: 213330,
        len: 542,
        anchor: (11.615, 23.22),
    },
    Sprite {
        start: 213872,
        len: 478,
        anchor: (11.565, 23.225),
    },
    Sprite {
        start: 214350,
        len: 426,
        anchor: (11.875, 23.21),
    },
    Sprite {
        start: 214776,
        len: 314,
        anchor: (12.335, 23.185),
    },
    Sprite {
        start: 215090,
        len: 94,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 215184,
        len: 94,
        anchor: (12.71, 23.16),
    },
    Sprite {
        start: 215278,
        len: 324,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 215602,
        len: 404,
        anchor: (12.465, 23.205),
    },
    Sprite {
        start: 216006,
        len: 432,
        anchor: (11.985, 23.22),
    },
    Sprite {
        start: 216438,
        len: 450,
        anchor: (11.65, 23.225),
    },
    Sprite {
        start: 216888,
        len: 606,
        anchor: (12.485, 23.21),
    },
    Sprite {
        start: 217494,
        len: 746,
        anchor: (12.335, 23.185),
    },
    Sprite {
        start: 218240,
        len: 1192,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 219432,
        len: 1146,
        anchor: (12.71, 23.16),
    },
    Sprite {
        start: 220578,
        len: 1142,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 221720,
        len: 1072,
        anchor: (11.98, 23.205),
    },
    Sprite {
        start: 222792,
        len: 960,
        anchor: (11.615, 23.22),
    },
    Sprite {
        start: 223752,
        len: 962,
        anchor: (11.565, 23.225),
    },
    Sprite {
        start: 224714,
        len: 902,
        anchor: (11.875, 23.21),
    },
    Sprite {
        start: 225616,
        len: 986,
        anchor: (12.335, 23.185),
    },
    Sprite {
        start: 226602,
        len: 986,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 227588,
        len: 954,
        anchor: (12.71, 23.16),
    },
    Sprite {
        start: 228542,
        len: 1000,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 229542,
        len: 956,
        anchor: (12.465, 23.205),
    },
    Sprite {
        start: 230498,
        len: 930,
        anchor: (11.985, 23.22),
    },
    Sprite {
        start: 231428,
        len: 940,
        anchor: (11.65, 23.225),
    },
    Sprite {
        start: 232368,
        len: 1042,
        anchor: (12.485, 23.21),
    },
    Sprite {
        start: 233410,
        len: 1118,
        anchor: (12.335, 23.185),
    },
    Sprite {
        start: 234528,
        len: 732,
        anchor: (14.605, 25.56),
    },
    Sprite {
        start: 235260,
        len: 752,
        anchor: (14.64, 25.57),
    },
    Sprite {
        start: 236012,
        len: 772,
        anchor: (14.605, 25.56),
    },
    Sprite {
        start: 236784,
        len: 748,
        anchor: (14.64, 25.57),
    },
    Sprite {
        start: 237532,
        len: 786,
        anchor: (14.605, 25.56),
    },
    Sprite {
        start: 238318,
        len: 816,
        anchor: (14.64, 25.57),
    },
    Sprite {
        start: 239134,
        len: 778,
        anchor: (14.605, 25.56),
    },
    Sprite {
        start: 239912,
        len: 766,
        anchor: (14.64, 25.57),
    },
    Sprite {
        start: 240678,
        len: 748,
        anchor: (14.605, 25.56),
    },
    Sprite {
        start: 241426,
        len: 748,
        anchor: (14.64, 25.57),
    },
    Sprite {
        start: 242174,
        len: 750,
        anchor: (14.605, 25.56),
    },
    Sprite {
        start: 242924,
        len: 760,
        anchor: (14.64, 25.57),
    },
    Sprite {
        start: 243684,
        len: 778,
        anchor: (14.605, 25.56),
    },
    Sprite {
        start: 244462,
        len: 794,
        anchor: (14.64, 25.57),
    },
    Sprite {
        start: 245256,
        len: 810,
        anchor: (14.605, 25.56),
    },
    Sprite {
        start: 246066,
        len: 778,
        anchor: (14.64, 25.57),
    },
    Sprite {
        start: 246844,
        len: 1434,
        anchor: (14.605, 25.56),
    },
    Sprite {
        start: 248278,
        len: 1462,
        anchor: (14.64, 25.57),
    },
    Sprite {
        start: 249740,
        len: 1464,
        anchor: (14.605, 25.56),
    },
    Sprite {
        start: 251204,
        len: 1456,
        anchor: (14.64, 25.57),
    },
    Sprite {
        start: 252660,
        len: 1436,
        anchor: (14.605, 25.56),
    },
    Sprite {
        start: 254096,
        len: 1482,
        anchor: (14.64, 25.57),
    },
    Sprite {
        start: 255578,
        len: 1458,
        anchor: (14.605, 25.56),
    },
    Sprite {
        start: 257036,
        len: 1498,
        anchor: (14.64, 25.57),
    },
    Sprite {
        start: 258534,
        len: 1464,
        anchor: (14.605, 25.56),
    },
    Sprite {
        start: 259998,
        len: 1424,
        anchor: (14.64, 25.57),
    },
    Sprite {
        start: 261422,
        len: 1466,
        anchor: (14.605, 25.56),
    },
    Sprite {
        start: 262888,
        len: 1458,
        anchor: (14.64, 25.57),
    },
    Sprite {
        start: 264346,
        len: 1454,
        anchor: (14.605, 25.56),
    },
    Sprite {
        start: 265800,
        len: 1476,
        anchor: (14.64, 25.57),
    },
    Sprite {
        start: 267276,
        len: 1462,
        anchor: (14.605, 25.56),
    },
    Sprite {
        start: 268738,
        len: 1456,
        anchor: (14.64, 25.57),
    },
    Sprite {
        start: 270194,
        len: 1150,
        anchor: (12.675, 23.595),
    },
    Sprite {
        start: 271344,
        len: 1058,
        anchor: (12.71, 23.59),
    },
    Sprite {
        start: 272402,
        len: 1044,
        anchor: (12.435, 23.61),
    },
    Sprite {
        start: 273446,
        len: 1016,
        anchor: (12.165, 23.635),
    },
    Sprite {
        start: 274462,
        len: 1032,
        anchor: (11.805, 23.65),
    },
    Sprite {
        start: 275494,
        len: 1096,
        anchor: (11.755, 23.655),
    },
    Sprite {
        start: 276590,
        len: 1138,
        anchor: (12.815, 23.64),
    },
    Sprite {
        start: 277728,
        len: 1158,
        anchor: (13.745, 23.615),
    },
    Sprite {
        start: 278886,
        len: 1154,
        anchor: (12.86, 23.595),
    },
    Sprite {
        start: 280040,
        len: 1204,
        anchor: (12.71, 23.59),
    },
    Sprite {
        start: 281244,
        len: 1140,
        anchor: (12.435, 23.61),
    },
    Sprite {
        start: 282384,
        len: 1166,
        anchor: (12.465, 23.635),
    },
    Sprite {
        start: 283550,
        len: 1098,
        anchor: (11.985, 23.65),
    },
    Sprite {
        start: 284648,
        len: 1116,
        anchor: (11.65, 23.655),
    },
    Sprite {
        start: 285764,
        len: 1110,
        anchor: (12.485, 23.64),
    },
    Sprite {
        start: 286874,
        len: 1128,
        anchor: (12.335, 23.615),
    },
    Sprite {
        start: 288002,
        len: 402,
        anchor: (12.675, 23.595),
    },
    Sprite {
        start: 288404,
        len: 376,
        anchor: (12.71, 23.59),
    },
    Sprite {
        start: 288780,
        len: 344,
        anchor: (12.435, 23.61),
    },
    Sprite {
        start: 289124,
        len: 320,
        anchor: (12.165, 23.635),
    },
    Sprite {
        start: 289444,
        len: 302,
        anchor: (11.805, 23.65),
    },
    Sprite {
        start: 289746,
        len: 208,
        anchor: (11.755, 23.655),
    },
    Sprite {
        start: 289954,
        len: 190,
        anchor: (12.815, 23.64),
    },
    Sprite {
        start: 290144,
        len: 94,
        anchor: (13.745, 23.615),
    },
    Sprite {
        start: 290238,
        len: 94,
        anchor: (12.86, 23.595),
    },
    Sprite {
        start: 290332,
        len: 94,
        anchor: (12.71, 23.59),
    },
    Sprite {
        start: 290426,
        len: 148,
        anchor: (12.435, 23.61),
    },
    Sprite {
        start: 290574,
        len: 198,
        anchor: (12.465, 23.635),
    },
    Sprite {
        start: 290772,
        len: 192,
        anchor: (11.985, 23.65),
    },
    Sprite {
        start: 290964,
        len: 244,
        anchor: (11.65, 23.655),
    },
    Sprite {
        start: 291208,
        len: 324,
        anchor: (12.485, 23.64),
    },
    Sprite {
        start: 291532,
        len: 396,
        anchor: (12.335, 23.615),
    },
    Sprite {
        start: 291928,
        len: 1270,
        anchor: (13.24, 24.29),
    },
    Sprite {
        start: 293198,
        len: 1308,
        anchor: (13.275, 24.3),
    },
    Sprite {
        start: 294506,
        len: 1248,
        anchor: (13.025, 24.215),
    },
    Sprite {
        start: 295754,
        len: 1190,
        anchor: (12.685, 24.16),
    },
    Sprite {
        start: 296944,
        len: 1186,
        anchor: (12.32, 24.18),
    },
    Sprite {
        start: 298130,
        len: 1270,
        anchor: (12.315, 24.18),
    },
    Sprite {
        start: 299400,
        len: 1426,
        anchor: (12.58, 24.165),
    },
    Sprite {
        start: 300826,
        len: 1564,
        anchor: (12.93, 24.185),
    },
    Sprite {
        start: 302390,
        len: 1600,
        anchor: (13.24, 24.29),
    },
    Sprite {
        start: 303990,
        len: 1610,
        anchor: (13.275, 24.3),
    },
    Sprite {
        start: 305600,
        len: 1598,
        anchor: (13.025, 24.215),
    },
    Sprite {
        start: 307198,
        len: 1500,
        anchor: (12.87, 24.16),
    },
    Sprite {
        start: 308698,
        len: 1448,
        anchor: (12.9, 24.18),
    },
    Sprite {
        start: 310146,
        len: 1258,
        anchor: (12.955, 24.18),
    },
    Sprite {
        start: 311404,
        len: 1218,
        anchor: (12.97, 24.165),
    },
    Sprite {
        start: 312622,
        len: 1238,
        anchor: (12.93, 24.185),
    },
    Sprite {
        start: 313860,
        len: 1626,
        anchor: (13.24, 24.29),
    },
    Sprite {
        start: 315486,
        len: 1674,
        anchor: (13.275, 24.3),
    },
    Sprite {
        start: 317160,
        len: 1612,
        anchor: (13.025, 24.215),
    },
    Sprite {
        start: 318772,
        len: 1610,
        anchor: (12.685, 24.16),
    },
    Sprite {
        start: 320382,
        len: 1588,
        anchor: (12.32, 24.18),
    },
    Sprite {
        start: 321970,
        len: 1576,
        anchor: (12.315, 24.18),
    },
    Sprite {
        start: 323546,
        len: 1604,
        anchor: (12.58, 24.165),
    },
    Sprite {
        start: 325150,
        len: 1532,
        anchor: (12.93, 24.185),
    },
    Sprite {
        start: 326682,
        len: 1426,
        anchor: (13.24, 24.29),
    },
    Sprite {
        start: 328108,
        len: 1384,
        anchor: (13.275, 24.3),
    },
    Sprite {
        start: 329492,
        len: 1516,
        anchor: (13.025, 24.215),
    },
    Sprite {
        start: 331008,
        len: 1600,
        anchor: (12.87, 24.16),
    },
    Sprite {
        start: 332608,
        len: 1604,
        anchor: (12.9, 24.18),
    },
    Sprite {
        start: 334212,
        len: 1582,
        anchor: (12.955, 24.18),
    },
    Sprite {
        start: 335794,
        len: 1502,
        anchor: (12.97, 24.165),
    },
    Sprite {
        start: 337296,
        len: 1540,
        anchor: (12.93, 24.185),
    },
    Sprite {
        start: 338836,
        len: 716,
        anchor: (12.675, 23.45),
    },
    Sprite {
        start: 339552,
        len: 786,
        anchor: (12.71, 23.45),
    },
    Sprite {
        start: 340338,
        len: 774,
        anchor: (12.435, 23.465),
    },
    Sprite {
        start: 341112,
        len: 792,
        anchor: (11.98, 23.49),
    },
    Sprite {
        start: 341904,
        len: 796,
        anchor: (11.615, 23.975),
    },
    Sprite {
        start: 342700,
        len: 870,
        anchor: (11.565, 24.725),
    },
    Sprite {
        start: 343570,
        len: 850,
        anchor: (11.875, 25.23),
    },
    Sprite {
        start: 344420,
        len: 846,
        anchor: (12.335, 25.47),
    },
    Sprite {
        start: 345266,
        len: 812,
        anchor: (12.675, 25.365),
    },
    Sprite {
        start: 346078,
        len: 878,
        anchor: (12.71, 24.985),
    },
    Sprite {
        start: 346956,
        len: 756,
        anchor: (12.435, 24.4),
    },
    Sprite {
        start: 347712,
        len: 746,
        anchor: (12.465, 23.665),
    },
    Sprite {
        start: 348458,
        len: 778,
        anchor: (11.985, 23.51),
    },
    Sprite {
        start: 349236,
        len: 744,
        anchor: (11.65, 23.51),
    },
    Sprite {
        start: 349980,
        len: 662,
        anchor: (12.485, 23.495),
    },
    Sprite {
        start: 350642,
        len: 714,
        anchor: (12.335, 23.47),
    },
    Sprite {
        start: 351356,
        len: 498,
        anchor: (12.675, 26.715),
    },
    Sprite {
        start: 351854,
        len: 520,
        anchor: (12.71, 26.65),
    },
    Sprite {
        start: 352374,
        len: 516,
        anchor: (12.435, 26.915),
    },
    Sprite {
        start: 352890,
        len: 460,
        anchor: (11.98, 27.145),
    },
    Sprite {
        start: 353350,
        len: 408,
        anchor: (11.615, 27.285),
    },
    Sprite {
        start: 353758,
        len: 402,
        anchor: (11.565, 27.3),
    },
    Sprite {
        start: 354160,
        len: 434,
        anchor: (11.875, 27.19),
    },
    Sprite {
        start: 354594,
        len: 504,
        anchor: (12.335, 26.975),
    },
    Sprite {
        start: 355098,
        len: 514,
        anchor: (12.675, 26.715),
    },
    Sprite {
        start: 355612,
        len: 514,
        anchor: (12.71, 26.65),
    },
    Sprite {
        start: 356126,
        len: 524,
        anchor: (12.435, 26.915),
    },
    Sprite {
        start: 356650,
        len: 446,
        anchor: (12.465, 27.145),
    },
    Sprite {
        start: 357096,
        len: 410,
        anchor: (11.985, 27.285),
    },
    Sprite {
        start: 357506,
        len: 410,
        anchor: (11.65, 27.3),
    },
    Sprite {
        start: 357916,
        len: 438,
        anchor: (12.485, 27.19),
    },
    Sprite {
        start: 358354,
        len: 510,
        anchor: (12.335, 26.975),
    },
    Sprite {
        start: 358864,
        len: 588,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 359452,
        len: 518,
        anchor: (12.71, 23.16),
    },
    Sprite {
        start: 359970,
        len: 476,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 360446,
        len: 396,
        anchor: (11.98, 23.205),
    },
    Sprite {
        start: 360842,
        len: 406,
        anchor: (11.615, 23.22),
    },
    Sprite {
        start: 361248,
        len: 304,
        anchor: (11.565, 23.225),
    },
    Sprite {
        start: 361552,
        len: 138,
        anchor: (11.875, 23.21),
    },
    Sprite {
        start: 361690,
        len: 178,
        anchor: (12.335, 23.185),
    },
    Sprite {
        start: 361868,
        len: 378,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 362246,
        len: 372,
        anchor: (12.71, 23.16),
    },
    Sprite {
        start: 362618,
        len: 448,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 363066,
        len: 428,
        anchor: (12.465, 23.205),
    },
    Sprite {
        start: 363494,
        len: 518,
        anchor: (11.985, 23.22),
    },
    Sprite {
        start: 364012,
        len: 592,
        anchor: (11.65, 23.225),
    },
    Sprite {
        start: 364604,
        len: 642,
        anchor: (12.485, 23.21),
    },
    Sprite {
        start: 365246,
        len: 608,
        anchor: (12.335, 23.185),
    },
    Sprite {
        start: 365854,
        len: 374,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 366228,
        len: 358,
        anchor: (12.71, 23.16),
    },
    Sprite {
        start: 366586,
        len: 318,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 366904,
        len: 288,
        anchor: (11.98, 23.205),
    },
    Sprite {
        start: 367192,
        len: 92,
        anchor: (11.615, 23.22),
    },
    Sprite {
        start: 367284,
        len: 92,
        anchor: (11.565, 23.225),
    },
    Sprite {
        start: 367376,
        len: 94,
        anchor: (11.875, 23.21),
    },
    Sprite {
        start: 367470,
        len: 94,
        anchor: (12.335, 23.185),
    },
    Sprite {
        start: 367564,
        len: 94,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 367658,
        len: 94,
        anchor: (12.71, 23.16),
    },
    Sprite {
        start: 367752,
        len: 302,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 368054,
        len: 342,
        anchor: (12.465, 23.205),
    },
    Sprite {
        start: 368396,
        len: 388,
        anchor: (11.985, 23.22),
    },
    Sprite {
        start: 368784,
        len: 378,
        anchor: (11.65, 23.225),
    },
    Sprite {
        start: 369162,
        len: 390,
        anchor: (12.485, 23.21),
    },
    Sprite {
        start: 369552,
        len: 404,
        anchor: (12.335, 23.185),
    },
    Sprite {
        start: 369956,
        len: 682,
        anchor: (15.46, 23.53),
    },
    Sprite {
        start: 370638,
        len: 722,
        anchor: (15.625, 23.5),
    },
    Sprite {
        start: 371360,
        len: 742,
        anchor: (14.475, 23.635),
    },
    Sprite {
        start: 372102,
        len: 482,
        anchor: (11.98, 23.875),
    },
    Sprite {
        start: 372584,
        len: 538,
        anchor: (11.615, 24.055),
    },
    Sprite {
        start: 373122,
        len: 560,
        anchor: (11.565, 24.075),
    },
    Sprite {
        start: 373682,
        len: 548,
        anchor: (11.875, 23.92),
    },
    Sprite {
        start: 374230,
        len: 676,
        anchor: (13.905, 23.705),
    },
    Sprite {
        start: 374906,
        len: 672,
        anchor: (15.46, 23.53),
    },
    Sprite {
        start: 375578,
        len: 726,
        anchor: (15.625, 23.5),
    },
    Sprite {
        start: 376304,
        len: 738,
        anchor: (14.475, 23.635),
    },
    Sprite {
        start: 377042,
        len: 568,
        anchor: (12.465, 23.875),
    },
    Sprite {
        start: 377610,
        len: 520,
        anchor: (11.985, 24.055),
    },
    Sprite {
        start: 378130,
        len: 500,
        anchor: (11.65, 24.075),
    },
    Sprite {
        start: 378630,
        len: 556,
        anchor: (12.485, 23.92),
    },
    Sprite {
        start: 379186,
        len: 714,
        anchor: (13.905, 23.705),
    },
    Sprite {
        start: 379900,
        len: 1378,
        anchor: (15.46, 23.53),
    },
    Sprite {
        start: 381278,
        len: 1432,
        anchor: (15.625, 23.5),
    },
    Sprite {
        start: 382710,
        len: 1390,
        anchor: (14.475, 23.635),
    },
    Sprite {
        start: 384100,
        len: 1156,
        anchor: (11.98, 23.875),
    },
    Sprite {
        start: 385256,
        len: 900,
        anchor: (11.615, 24.055),
    },
    Sprite {
        start: 386156,
        len: 794,
        anchor: (11.565, 24.075),
    },
    Sprite {
        start: 386950,
        len: 1054,
        anchor: (11.875, 23.92),
    },
    Sprite {
        start: 388004,
        len: 1416,
        anchor: (13.905, 23.705),
    },
    Sprite {
        start: 389420,
        len: 1406,
        anchor: (15.46, 23.53),
    },
    Sprite {
        start: 390826,
        len: 1388,
        anchor: (15.625, 23.5),
    },
    Sprite {
        start: 392214,
        len: 1422,
        anchor: (14.475, 23.635),
    },
    Sprite {
        start: 393636,
        len: 1154,
        anchor: (12.465, 23.875),
    },
    Sprite {
        start: 394790,
        len: 954,
        anchor: (11.985, 24.055),
    },
    Sprite {
        start: 395744,
        len: 834,
        anchor: (11.65, 24.075),
    },
    Sprite {
        start: 396578,
        len: 1066,
        anchor: (12.485, 23.92),
    },
    Sprite {
        start: 397644,
        len: 1378,
        anchor: (13.905, 23.705),
    },
    Sprite {
        start: 399022,
        len: 292,
        anchor: (12.675, 28.935),
    },
    Sprite {
        start: 399314,
        len: 290,
        anchor: (12.71, 29.145),
    },
    Sprite {
        start: 399604,
        len: 272,
        anchor: (12.435, 29.33),
    },
    Sprite {
        start: 399876,
        len: 260,
        anchor: (11.98, 29.47),
    },
    Sprite {
        start: 400136,
        len: 274,
        anchor: (11.615, 29.535),
    },
    Sprite {
        start: 400410,
        len: 254,
        anchor: (11.565, 29.52),
    },
    Sprite {
        start: 400664,
        len: 252,
        anchor: (11.875, 29.425),
    },
    Sprite {
        start: 400916,
        len: 268,
        anchor: (12.335, 29.265),
    },
    Sprite {
        start: 401184,
        len: 246,
        anchor: (12.675, 29.065),
    },
    Sprite {
        start: 401430,
        len: 242,
        anchor: (12.71, 28.855),
    },
    Sprite {
        start: 401672,
        len: 272,
        anchor: (12.435, 28.665),
    },
    Sprite {
        start: 401944,
        len: 266,
        anchor: (12.465, 28.53),
    },
    Sprite {
        start: 402210,
        len: 250,
        anchor: (11.985, 28.465),
    },
    Sprite {
        start: 402460,
        len: 236,
        anchor: (11.65, 28.48),
    },
    Sprite {
        start: 402696,
        len: 256,
        anchor: (12.485, 28.575),
    },
    Sprite {
        start: 402952,
        len: 278,
        anchor: (12.335, 28.735),
    },
    Sprite {
        start: 403230,
        len: 318,
        anchor: (12.675, 28.935),
    },
    Sprite {
        start: 403548,
        len: 344,
        anchor: (12.71, 29.145),
    },
    Sprite {
        start: 403892,
        len: 374,
        anchor: (12.435, 29.33),
    },
    Sprite {
        start: 404266,
        len: 302,
        anchor: (11.98, 29.47),
    },
    Sprite {
        start: 404568,
        len: 346,
        anchor: (11.615, 29.535),
    },
    Sprite {
        start: 404914,
        len: 236,
        anchor: (11.565, 29.52),
    },
    Sprite {
        start: 405150,
        len: 278,
        anchor: (11.875, 29.425),
    },
    Sprite {
        start: 405428,
        len: 300,
        anchor: (12.335, 29.265),
    },
    Sprite {
        start: 405728,
        len: 336,
        anchor: (12.675, 29.065),
    },
    Sprite {
        start: 406064,
        len: 346,
        anchor: (12.71, 28.855),
    },
    Sprite {
        start: 406410,
        len: 292,
        anchor: (12.435, 28.665),
    },
    Sprite {
        start: 406702,
        len: 304,
        anchor: (12.465, 28.53),
    },
    Sprite {
        start: 407006,
        len: 266,
        anchor: (11.985, 28.465),
    },
    Sprite {
        start: 407272,
        len: 260,
        anchor: (11.65, 28.48),
    },
    Sprite {
        start: 407532,
        len: 294,
        anchor: (12.485, 28.575),
    },
    Sprite {
        start: 407826,
        len: 318,
        anchor: (12.335, 28.735),
    },
    Sprite {
        start: 408144,
        len: 804,
        anchor: (12.675, 29.32),
    },
    Sprite {
        start: 408948,
        len: 710,
        anchor: (12.71, 29.33),
    },
    Sprite {
        start: 409658,
        len: 638,
        anchor: (12.435, 29.26),
    },
    Sprite {
        start: 410296,
        len: 642,
        anchor: (11.98, 29.5),
    },
    Sprite {
        start: 410938,
        len: 710,
        anchor: (11.615, 29.645),
    },
    Sprite {
        start: 411648,
        len: 616,
        anchor: (11.565, 29.66),
    },
    Sprite {
        start: 412264,
        len: 630,
        anchor: (11.875, 29.55),
    },
    Sprite {
        start: 412894,
        len: 660,
        anchor: (12.335, 29.32),
    },
    Sprite {
        start: 413554,
        len: 792,
        anchor: (12.675, 29.32),
    },
    Sprite {
        start: 414346,
        len: 710,
        anchor: (12.71, 29.33),
    },
    Sprite {
        start: 415056,
        len: 680,
        anchor: (12.435, 29.26),
    },
    Sprite {
        start: 415736,
        len: 586,
        anchor: (12.465, 29.5),
    },
    Sprite {
        start: 416322,
        len: 616,
        anchor: (11.985, 29.645),
    },
    Sprite {
        start: 416938,
        len: 734,
        anchor: (11.65, 29.66),
    },
    Sprite {
        start: 417672,
        len: 646,
        anchor: (12.485, 29.55),
    },
    Sprite {
        start: 418318,
        len: 632,
        anchor: (12.335, 29.32),
    },
    Sprite {
        start: 418950,
        len: 580,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 419530,
        len: 496,
        anchor: (12.71, 23.16),
    },
    Sprite {
        start: 420026,
        len: 422,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 420448,
        len: 346,
        anchor: (11.98, 23.205),
    },
    Sprite {
        start: 420794,
        len: 368,
        anchor: (11.615, 23.22),
    },
    Sprite {
        start: 421162,
        len: 298,
        anchor: (11.565, 23.225),
    },
    Sprite {
        start: 421460,
        len: 94,
        anchor: (11.875, 23.21),
    },
    Sprite {
        start: 421554,
        len: 94,
        anchor: (12.335, 23.185),
    },
    Sprite {
        start: 421648,
        len: 326,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 421974,
        len: 362,
        anchor: (12.71, 23.16),
    },
    Sprite {
        start: 422336,
        len: 400,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 422736,
        len: 380,
        anchor: (12.465, 23.205),
    },
    Sprite {
        start: 423116,
        len: 420,
        anchor: (11.985, 23.22),
    },
    Sprite {
        start: 423536,
        len: 500,
        anchor: (11.65, 23.225),
    },
    Sprite {
        start: 424036,
        len: 532,
        anchor: (12.485, 23.21),
    },
    Sprite {
        start: 424568,
        len: 492,
        anchor: (12.335, 23.185),
    },
    Sprite {
        start: 425060,
        len: 1096,
        anchor: (12.85, 23.165),
    },
    Sprite {
        start: 426156,
        len: 1122,
        anchor: (13.02, 23.16),
    },
    Sprite {
        start: 427278,
        len: 1030,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 428308,
        len: 916,
        anchor: (11.98, 23.205),
    },
    Sprite {
        start: 429224,
        len: 716,
        anchor: (11.615, 23.22),
    },
    Sprite {
        start: 429940,
        len: 626,
        anchor: (11.565, 23.225),
    },
    Sprite {
        start: 430566,
        len: 478,
        anchor: (11.875, 23.21),
    },
    Sprite {
        start: 431044,
        len: 380,
        anchor: (12.335, 23.185),
    },
    Sprite {
        start: 431424,
        len: 288,
        anchor: (12.775, 23.165),
    },
    Sprite {
        start: 431712,
        len: 280,
        anchor: (13.07, 23.16),
    },
    Sprite {
        start: 431992,
        len: 242,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 432234,
        len: 438,
        anchor: (13.6, 23.205),
    },
    Sprite {
        start: 432672,
        len: 472,
        anchor: (12.99, 23.22),
    },
    Sprite {
        start: 433144,
        len: 616,
        anchor: (13.025, 23.225),
    },
    Sprite {
        start: 433760,
        len: 882,
        anchor: (13.63, 23.21),
    },
    Sprite {
        start: 434642,
        len: 976,
        anchor: (12.76, 23.185),
    },
    Sprite {
        start: 435618,
        len: 1082,
        anchor: (12.85, 23.165),
    },
    Sprite {
        start: 436700,
        len: 1132,
        anchor: (13.02, 23.16),
    },
    Sprite {
        start: 437832,
        len: 1052,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 438884,
        len: 906,
        anchor: (11.98, 23.205),
    },
    Sprite {
        start: 439790,
        len: 718,
        anchor: (11.615, 23.22),
    },
    Sprite {
        start: 440508,
        len: 594,
        anchor: (11.565, 23.225),
    },
    Sprite {
        start: 441102,
        len: 470,
        anchor: (11.875, 23.21),
    },
    Sprite {
        start: 441572,
        len: 380,
        anchor: (12.335, 23.185),
    },
    Sprite {
        start: 441952,
        len: 288,
        anchor: (12.775, 23.165),
    },
    Sprite {
        start: 442240,
        len: 280,
        anchor: (13.07, 23.16),
    },
    Sprite {
        start: 442520,
        len: 242,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 442762,
        len: 448,
        anchor: (13.6, 23.205),
    },
    Sprite {
        start: 443210,
        len: 472,
        anchor: (12.99, 23.22),
    },
    Sprite {
        start: 443682,
        len: 642,
        anchor: (13.025, 23.225),
    },
    Sprite {
        start: 444324,
        len: 854,
        anchor: (13.63, 23.21),
    },
    Sprite {
        start: 445178,
        len: 972,
        anchor: (12.76, 23.185),
    },
    Sprite {
        start: 446150,
        len: 508,
        anchor: (12.85, 23.165),
    },
    Sprite {
        start: 446658,
        len: 558,
        anchor: (13.02, 23.16),
    },
    Sprite {
        start: 447216,
        len: 580,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 447796,
        len: 566,
        anchor: (11.98, 23.205),
    },
    Sprite {
        start: 448362,
        len: 388,
        anchor: (11.615, 23.22),
    },
    Sprite {
        start: 448750,
        len: 384,
        anchor: (11.565, 23.225),
    },
    Sprite {
        start: 449134,
        len: 248,
        anchor: (11.875, 23.21),
    },
    Sprite {
        start: 449382,
        len: 94,
        anchor: (12.335, 23.185),
    },
    Sprite {
        start: 449476,
        len: 94,
        anchor: (12.775, 23.165),
    },
    Sprite {
        start: 449570,
        len: 94,
        anchor: (13.07, 23.16),
    },
    Sprite {
        start: 449664,
        len: 94,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 449758,
        len: 200,
        anchor: (13.6, 23.205),
    },
    Sprite {
        start: 449958,
        len: 128,
        anchor: (12.99, 23.22),
    },
    Sprite {
        start: 450086,
        len: 372,
        anchor: (13.025, 23.225),
    },
    Sprite {
        start: 450458,
        len: 444,
        anchor: (13.63, 23.21),
    },
    Sprite {
        start: 450902,
        len: 536,
        anchor: (12.76, 23.185),
    },
    Sprite {
        start: 451438,
        len: 442,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 451880,
        len: 448,
        anchor: (12.71, 23.16),
    },
    Sprite {
        start: 452328,
        len: 468,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 452796,
        len: 468,
        anchor: (11.98, 23.205),
    },
    Sprite {
        start: 453264,
        len: 370,
        anchor: (11.615, 23.22),
    },
    Sprite {
        start: 453634,
        len: 300,
        anchor: (11.565, 23.225),
    },
    Sprite {
        start: 453934,
        len: 280,
        anchor: (11.875, 23.21),
    },
    Sprite {
        start: 454214,
        len: 94,
        anchor: (12.335, 23.185),
    },
    Sprite {
        start: 454308,
        len: 94,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 454402,
        len: 94,
        anchor: (12.71, 23.16),
    },
    Sprite {
        start: 454496,
        len: 94,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 454590,
        len: 186,
        anchor: (12.465, 23.205),
    },
    Sprite {
        start: 454776,
        len: 272,
        anchor: (12.7, 23.22),
    },
    Sprite {
        start: 455048,
        len: 354,
        anchor: (12.6, 23.225),
    },
    Sprite {
        start: 455402,
        len: 412,
        anchor: (12.485, 23.21),
    },
    Sprite {
        start: 455814,
        len: 466,
        anchor: (12.335, 23.185),
    },
    Sprite {
        start: 456280,
        len: 964,
        anchor: (12.92, 23.165),
    },
    Sprite {
        start: 457244,
        len: 954,
        anchor: (12.96, 23.16),
    },
    Sprite {
        start: 458198,
        len: 930,
        anchor: (12.7, 23.18),
    },
    Sprite {
        start: 459128,
        len: 952,
        anchor: (12.265, 23.205),
    },
    Sprite {
        start: 460080,
        len: 914,
        anchor: (11.92, 23.22),
    },
    Sprite {
        start: 460994,
        len: 902,
        anchor: (11.87, 23.225),
    },
    Sprite {
        start: 461896,
        len: 930,
        anchor: (12.165, 23.21),
    },
    Sprite {
        start: 462826,
        len: 860,
        anchor: (12.6, 23.185),
    },
    Sprite {
        start: 463686,
        len: 848,
        anchor: (12.92, 23.165),
    },
    Sprite {
        start: 464534,
        len: 874,
        anchor: (12.96, 23.16),
    },
    Sprite {
        start: 465408,
        len: 884,
        anchor: (12.7, 23.18),
    },
    Sprite {
        start: 466292,
        len: 940,
        anchor: (12.465, 23.205),
    },
    Sprite {
        start: 467232,
        len: 998,
        anchor: (12.265, 23.22),
    },
    Sprite {
        start: 468230,
        len: 984,
        anchor: (11.87, 23.225),
    },
    Sprite {
        start: 469214,
        len: 1062,
        anchor: (12.485, 23.21),
    },
    Sprite {
        start: 470276,
        len: 962,
        anchor: (12.6, 23.185),
    },
    Sprite {
        start: 471238,
        len: 706,
        anchor: (5.485, 7.68),
    },
    Sprite {
        start: 471944,
        len: 734,
        anchor: (5.485, 7.68),
    },
    Sprite {
        start: 472678,
        len: 744,
        anchor: (5.485, 7.68),
    },
    Sprite {
        start: 473422,
        len: 762,
        anchor: (5.485, 7.68),
    },
    Sprite {
        start: 474184,
        len: 790,
        anchor: (5.485, 7.68),
    },
    Sprite {
        start: 474974,
        len: 402,
        anchor: (3.665, 4.61),
    },
    Sprite {
        start: 475376,
        len: 416,
        anchor: (3.665, 4.61),
    },
    Sprite {
        start: 475792,
        len: 416,
        anchor: (3.665, 4.61),
    },
    Sprite {
        start: 476208,
        len: 430,
        anchor: (3.665, 4.61),
    },
    Sprite {
        start: 476638,
        len: 434,
        anchor: (3.665, 4.61),
    },
    Sprite {
        start: 477072,
        len: 172,
        anchor: (1.44, 1.44),
    },
    Sprite {
        start: 477244,
        len: 252,
        anchor: (2.16, 2.16),
    },
    Sprite {
        start: 477496,
        len: 214,
        anchor: (1.56, 1.895),
    },
    Sprite {
        start: 477710,
        len: 406,
        anchor: (1.955, 5.54),
    },
    Sprite {
        start: 478116,
        len: 296,
        anchor: (2.64, 2.415),
    },
    Sprite {
        start: 478412,
        len: 240,
        anchor: (1.8, 1.8),
    },
    Sprite {
        start: 478652,
        len: 230,
        anchor: (1.8, 1.8),
    },
    Sprite {
        start: 478882,
        len: 382,
        anchor: (1.8, 4.475),
    },
    Sprite {
        start: 479264,
        len: 342,
        anchor: (2.875, 2.955),
    },
    Sprite {
        start: 479606,
        len: 350,
        anchor: (2.95, 2.77),
    },
    Sprite {
        start: 479956,
        len: 364,
        anchor: (2.88, 4.025),
    },
    Sprite {
        start: 480320,
        len: 364,
        anchor: (2.88, 4.025),
    },
    Sprite {
        start: 480684,
        len: 298,
        anchor: (1.145, 4.41),
    },
    Sprite {
        start: 480982,
        len: 340,
        anchor: (2.27, 2.81),
    },
    Sprite {
        start: 481322,
        len: 244,
        anchor: (1.7, 1.635),
    },
    Sprite {
        start: 481566,
        len: 252,
        anchor: (1.58, 1.665),
    },
    Sprite {
        start: 481818,
        len: 210,
        anchor: (1.29, 1.615),
    },
    Sprite {
        start: 482028,
        len: 196,
        anchor: (1.445, 1.69),
    },
    Sprite {
        start: 482224,
        len: 294,
        anchor: (2.415, 2.79),
    },
    Sprite {
        start: 482518,
        len: 320,
        anchor: (2.815, 4.325),
    },
    Sprite {
        start: 482838,
        len: 374,
        anchor: (3.21, 5.865),
    },
    Sprite {
        start: 483212,
        len: 418,
        anchor: (3.61, 7.405),
    },
    Sprite {
        start: 483630,
        len: 644,
        anchor: (4.015, 9.905),
    },
    Sprite {
        start: 484274,
        len: 1806,
        anchor: (17.48, 17.48),
    },
    Sprite {
        start: 486080,
        len: 1720,
        anchor: (17.48, 17.48),
    },
    Sprite {
        start: 487800,
        len: 1580,
        anchor: (17.48, 17.48),
    },
    Sprite {
        start: 489380,
        len: 1588,
        anchor: (17.48, 17.48),
    },
    Sprite {
        start: 490968,
        len: 1614,
        anchor: (17.48, 17.48),
    },
    Sprite {
        start: 492582,
        len: 1720,
        anchor: (17.48, 17.48),
    },
    Sprite {
        start: 494302,
        len: 1716,
        anchor: (17.48, 17.48),
    },
    Sprite {
        start: 496018,
        len: 3126,
        anchor: (24.145, 24.145),
    },
    Sprite {
        start: 499144,
        len: 3142,
        anchor: (24.145, 24.145),
    },
    Sprite {
        start: 502286,
        len: 2946,
        anchor: (24.145, 24.145),
    },
    Sprite {
        start: 505232,
        len: 2874,
        anchor: (24.145, 24.145),
    },
    Sprite {
        start: 508106,
        len: 3114,
        anchor: (24.145, 24.145),
    },
    Sprite {
        start: 511220,
        len: 3088,
        anchor: (24.145, 24.145),
    },
    Sprite {
        start: 514308,
        len: 3116,
        anchor: (24.145, 24.145),
    },
    Sprite {
        start: 517424,
        len: 632,
        anchor: (4.27, 4.27),
    },
    Sprite {
        start: 518056,
        len: 664,
        anchor: (4.27, 4.27),
    },
    Sprite {
        start: 518720,
        len: 678,
        anchor: (4.27, 4.27),
    },
    Sprite {
        start: 519398,
        len: 688,
        anchor: (4.27, 4.27),
    },
    Sprite {
        start: 520086,
        len: 658,
        anchor: (4.27, 4.27),
    },
    Sprite {
        start: 520744,
        len: 646,
        anchor: (4.27, 4.27),
    },
    Sprite {
        start: 521390,
        len: 676,
        anchor: (4.27, 4.27),
    },
    Sprite {
        start: 522066,
        len: 636,
        anchor: (4.27, 4.27),
    },
    Sprite {
        start: 522702,
        len: 282,
        anchor: (11.0, 3.5),
    },
];

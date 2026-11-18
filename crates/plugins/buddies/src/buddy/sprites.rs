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
pub(super) const MOUTH_SMILE: usize = 192;
pub(super) const MOUTH_FLAT: usize = 208;
pub(super) const MOUTH_OPEN: usize = 224;
pub(super) const HEAD_BEANIE_BASE: usize = 240;
pub(super) const HEAD_BEANIE_TRIM: usize = 256;
pub(super) const HEAD_CAP_BASE: usize = 272;
pub(super) const HEAD_FEDORA_BASE: usize = 288;
pub(super) const HEAD_FEDORA_TRIM: usize = 304;
pub(super) const HEAD_BERET_BASE: usize = 320;
pub(super) const HEAD_GOGGLES_BASE: usize = 336;
pub(super) const HEAD_GOGGLES_TRIM: usize = 352;
pub(super) const HEAD_STRAW_BASE: usize = 368;
pub(super) const HEAD_STRAW_TRIM: usize = 384;
pub(super) const HEAD_BANDANA_BASE: usize = 400;
pub(super) const HEAD_BANDANA_TRIM: usize = 416;
pub(super) const HEAD_HELMET_BASE: usize = 432;
pub(super) const HEAD_HELMET_TRIM: usize = 448;
pub(super) const HEAD_SLICK_TRIM: usize = 464;
pub(super) const HEAD_SPRIG_TRIM: usize = 480;
pub(super) const HEAD_FLOWER_BASE: usize = 496;
pub(super) const HEAD_FLOWER_TRIM: usize = 512;
pub(super) const HEAD_HEADPHONES_BASE: usize = 528;
pub(super) const HEAD_HEADPHONES_TRIM: usize = 544;
pub(super) const HEAD_ANTENNA_BASE: usize = 560;
pub(super) const HEAD_ANTENNA_TRIM: usize = 576;
pub(super) const HEAD_CHEF_TRIM: usize = 592;
pub(super) const HEAD_BOW_BASE: usize = 608;
pub(super) const FACE_GLASSES_BASE: usize = 624;
pub(super) const FACE_SUNGLASSES_BASE: usize = 640;
pub(super) const FACE_SUNGLASSES_TRIM: usize = 656;
pub(super) const NECK_BOW_TIE_BASE: usize = 672;
pub(super) const NECK_SCARF_BASE: usize = 688;
pub(super) const LAPTOP: usize = 704;
pub(super) const NOTEPAD: usize = 709;
pub(super) const FX_SPARK: usize = 714;
pub(super) const FX_BUBBLE: usize = 715;
pub(super) const FX_DROP: usize = 716;
pub(super) const FX_NOTE: usize = 717;
pub(super) const FX_PUFF: usize = 718;
pub(super) const FX_DOT: usize = 719;
pub(super) const FX_DOT_LIT: usize = 720;
pub(super) const FX_BANG: usize = 721;
pub(super) const FX_STAR: usize = 722;
pub(super) const FX_HEART: usize = 723;
pub(super) const FX_BULB: usize = 724;
pub(super) const FX_BULB_LIT: usize = 725;
pub(super) const FX_SPEED: usize = 726;
pub(super) const CONFETTI: usize = 727;
pub(super) const SPROUT: usize = 731;
pub(super) const KART_FRONT: usize = 736;
pub(super) const KART_BACK: usize = 743;
pub(super) const WHEEL: usize = 750;
pub(super) const SHADOW: usize = 758;

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

pub(super) static SPRITES: [Sprite; 759] = [
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
        len: 398,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 80824,
        len: 380,
        anchor: (11.98, 23.205),
    },
    Sprite {
        start: 81204,
        len: 408,
        anchor: (11.615, 23.22),
    },
    Sprite {
        start: 81612,
        len: 308,
        anchor: (11.565, 23.225),
    },
    Sprite {
        start: 81920,
        len: 278,
        anchor: (11.875, 23.21),
    },
    Sprite {
        start: 82198,
        len: 94,
        anchor: (12.335, 23.185),
    },
    Sprite {
        start: 82292,
        len: 94,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 82386,
        len: 94,
        anchor: (12.71, 23.16),
    },
    Sprite {
        start: 82480,
        len: 94,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 82574,
        len: 94,
        anchor: (12.465, 23.205),
    },
    Sprite {
        start: 82668,
        len: 92,
        anchor: (11.985, 23.22),
    },
    Sprite {
        start: 82760,
        len: 258,
        anchor: (12.05, 23.225),
    },
    Sprite {
        start: 83018,
        len: 274,
        anchor: (12.485, 23.21),
    },
    Sprite {
        start: 83292,
        len: 342,
        anchor: (12.335, 23.185),
    },
    Sprite {
        start: 83634,
        len: 398,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 84032,
        len: 394,
        anchor: (12.71, 23.16),
    },
    Sprite {
        start: 84426,
        len: 388,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 84814,
        len: 366,
        anchor: (11.98, 23.205),
    },
    Sprite {
        start: 85180,
        len: 256,
        anchor: (11.615, 23.22),
    },
    Sprite {
        start: 85436,
        len: 92,
        anchor: (11.565, 23.225),
    },
    Sprite {
        start: 85528,
        len: 94,
        anchor: (11.875, 23.21),
    },
    Sprite {
        start: 85622,
        len: 94,
        anchor: (12.335, 23.185),
    },
    Sprite {
        start: 85716,
        len: 94,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 85810,
        len: 94,
        anchor: (12.71, 23.16),
    },
    Sprite {
        start: 85904,
        len: 94,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 85998,
        len: 232,
        anchor: (12.465, 23.205),
    },
    Sprite {
        start: 86230,
        len: 332,
        anchor: (12.085, 23.22),
    },
    Sprite {
        start: 86562,
        len: 378,
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
        len: 172,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 100372,
        len: 180,
        anchor: (12.71, 23.16),
    },
    Sprite {
        start: 100552,
        len: 190,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 100742,
        len: 164,
        anchor: (11.98, 23.205),
    },
    Sprite {
        start: 100906,
        len: 160,
        anchor: (11.615, 23.22),
    },
    Sprite {
        start: 101066,
        len: 148,
        anchor: (11.565, 23.225),
    },
    Sprite {
        start: 101214,
        len: 94,
        anchor: (11.875, 23.21),
    },
    Sprite {
        start: 101308,
        len: 94,
        anchor: (12.335, 23.185),
    },
    Sprite {
        start: 101402,
        len: 94,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 101496,
        len: 94,
        anchor: (12.71, 23.16),
    },
    Sprite {
        start: 101590,
        len: 94,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 101684,
        len: 94,
        anchor: (12.465, 23.205),
    },
    Sprite {
        start: 101778,
        len: 158,
        anchor: (11.985, 23.22),
    },
    Sprite {
        start: 101936,
        len: 216,
        anchor: (11.84, 23.225),
    },
    Sprite {
        start: 102152,
        len: 186,
        anchor: (12.485, 23.21),
    },
    Sprite {
        start: 102338,
        len: 176,
        anchor: (12.335, 23.185),
    },
    Sprite {
        start: 102514,
        len: 202,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 102716,
        len: 168,
        anchor: (12.71, 23.16),
    },
    Sprite {
        start: 102884,
        len: 170,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 103054,
        len: 184,
        anchor: (11.98, 23.205),
    },
    Sprite {
        start: 103238,
        len: 200,
        anchor: (11.615, 23.22),
    },
    Sprite {
        start: 103438,
        len: 198,
        anchor: (11.565, 23.225),
    },
    Sprite {
        start: 103636,
        len: 94,
        anchor: (11.875, 23.21),
    },
    Sprite {
        start: 103730,
        len: 94,
        anchor: (12.335, 23.185),
    },
    Sprite {
        start: 103824,
        len: 94,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 103918,
        len: 94,
        anchor: (12.71, 23.16),
    },
    Sprite {
        start: 104012,
        len: 94,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 104106,
        len: 94,
        anchor: (12.465, 23.205),
    },
    Sprite {
        start: 104200,
        len: 136,
        anchor: (11.985, 23.22),
    },
    Sprite {
        start: 104336,
        len: 206,
        anchor: (11.835, 23.225),
    },
    Sprite {
        start: 104542,
        len: 188,
        anchor: (12.485, 23.21),
    },
    Sprite {
        start: 104730,
        len: 180,
        anchor: (12.335, 23.185),
    },
    Sprite {
        start: 104910,
        len: 260,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 105170,
        len: 254,
        anchor: (12.71, 23.16),
    },
    Sprite {
        start: 105424,
        len: 226,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 105650,
        len: 260,
        anchor: (11.98, 23.205),
    },
    Sprite {
        start: 105910,
        len: 232,
        anchor: (11.615, 23.22),
    },
    Sprite {
        start: 106142,
        len: 108,
        anchor: (11.565, 23.225),
    },
    Sprite {
        start: 106250,
        len: 94,
        anchor: (11.875, 23.21),
    },
    Sprite {
        start: 106344,
        len: 94,
        anchor: (12.335, 23.185),
    },
    Sprite {
        start: 106438,
        len: 94,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 106532,
        len: 94,
        anchor: (12.71, 23.16),
    },
    Sprite {
        start: 106626,
        len: 94,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 106720,
        len: 94,
        anchor: (12.465, 23.205),
    },
    Sprite {
        start: 106814,
        len: 92,
        anchor: (11.985, 23.22),
    },
    Sprite {
        start: 106906,
        len: 224,
        anchor: (11.65, 23.225),
    },
    Sprite {
        start: 107130,
        len: 272,
        anchor: (12.485, 23.21),
    },
    Sprite {
        start: 107402,
        len: 212,
        anchor: (12.335, 23.185),
    },
    Sprite {
        start: 107614,
        len: 1224,
        anchor: (12.675, 26.38),
    },
    Sprite {
        start: 108838,
        len: 1212,
        anchor: (12.71, 26.38),
    },
    Sprite {
        start: 110050,
        len: 1186,
        anchor: (12.435, 26.38),
    },
    Sprite {
        start: 111236,
        len: 1166,
        anchor: (11.98, 26.38),
    },
    Sprite {
        start: 112402,
        len: 1018,
        anchor: (11.615, 26.38),
    },
    Sprite {
        start: 113420,
        len: 1038,
        anchor: (11.565, 26.38),
    },
    Sprite {
        start: 114458,
        len: 1100,
        anchor: (11.875, 26.38),
    },
    Sprite {
        start: 115558,
        len: 1132,
        anchor: (12.335, 26.38),
    },
    Sprite {
        start: 116690,
        len: 1138,
        anchor: (12.675, 26.38),
    },
    Sprite {
        start: 117828,
        len: 1196,
        anchor: (12.71, 26.38),
    },
    Sprite {
        start: 119024,
        len: 1170,
        anchor: (12.435, 26.38),
    },
    Sprite {
        start: 120194,
        len: 1100,
        anchor: (12.465, 26.38),
    },
    Sprite {
        start: 121294,
        len: 1120,
        anchor: (11.985, 26.38),
    },
    Sprite {
        start: 122414,
        len: 1134,
        anchor: (11.65, 26.38),
    },
    Sprite {
        start: 123548,
        len: 1080,
        anchor: (12.485, 26.38),
    },
    Sprite {
        start: 124628,
        len: 1130,
        anchor: (12.335, 26.38),
    },
    Sprite {
        start: 125758,
        len: 340,
        anchor: (12.675, 26.38),
    },
    Sprite {
        start: 126098,
        len: 384,
        anchor: (12.71, 26.38),
    },
    Sprite {
        start: 126482,
        len: 310,
        anchor: (12.435, 26.38),
    },
    Sprite {
        start: 126792,
        len: 308,
        anchor: (11.98, 26.38),
    },
    Sprite {
        start: 127100,
        len: 314,
        anchor: (11.615, 26.38),
    },
    Sprite {
        start: 127414,
        len: 354,
        anchor: (11.565, 26.38),
    },
    Sprite {
        start: 127768,
        len: 362,
        anchor: (11.875, 26.38),
    },
    Sprite {
        start: 128130,
        len: 336,
        anchor: (12.335, 26.38),
    },
    Sprite {
        start: 128466,
        len: 292,
        anchor: (12.675, 26.38),
    },
    Sprite {
        start: 128758,
        len: 302,
        anchor: (12.71, 26.38),
    },
    Sprite {
        start: 129060,
        len: 308,
        anchor: (12.435, 26.38),
    },
    Sprite {
        start: 129368,
        len: 316,
        anchor: (12.465, 26.38),
    },
    Sprite {
        start: 129684,
        len: 346,
        anchor: (11.985, 26.38),
    },
    Sprite {
        start: 130030,
        len: 304,
        anchor: (11.65, 26.38),
    },
    Sprite {
        start: 130334,
        len: 328,
        anchor: (12.485, 26.38),
    },
    Sprite {
        start: 130662,
        len: 328,
        anchor: (12.335, 26.38),
    },
    Sprite {
        start: 130990,
        len: 998,
        anchor: (12.675, 23.925),
    },
    Sprite {
        start: 131988,
        len: 964,
        anchor: (12.71, 23.925),
    },
    Sprite {
        start: 132952,
        len: 1040,
        anchor: (12.435, 23.925),
    },
    Sprite {
        start: 133992,
        len: 992,
        anchor: (11.98, 23.925),
    },
    Sprite {
        start: 134984,
        len: 1074,
        anchor: (11.615, 23.925),
    },
    Sprite {
        start: 136058,
        len: 1032,
        anchor: (11.565, 23.925),
    },
    Sprite {
        start: 137090,
        len: 1016,
        anchor: (11.875, 23.925),
    },
    Sprite {
        start: 138106,
        len: 990,
        anchor: (12.335, 23.925),
    },
    Sprite {
        start: 139096,
        len: 870,
        anchor: (12.675, 23.925),
    },
    Sprite {
        start: 139966,
        len: 896,
        anchor: (12.71, 23.925),
    },
    Sprite {
        start: 140862,
        len: 958,
        anchor: (12.435, 23.925),
    },
    Sprite {
        start: 141820,
        len: 1044,
        anchor: (14.47, 23.925),
    },
    Sprite {
        start: 142864,
        len: 954,
        anchor: (15.77, 23.925),
    },
    Sprite {
        start: 143818,
        len: 1062,
        anchor: (15.91, 23.925),
    },
    Sprite {
        start: 144880,
        len: 1060,
        anchor: (14.905, 23.925),
    },
    Sprite {
        start: 145940,
        len: 998,
        anchor: (12.62, 23.925),
    },
    Sprite {
        start: 146938,
        len: 1284,
        anchor: (12.675, 27.07),
    },
    Sprite {
        start: 148222,
        len: 1266,
        anchor: (12.71, 27.06),
    },
    Sprite {
        start: 149488,
        len: 1240,
        anchor: (12.435, 27.105),
    },
    Sprite {
        start: 150728,
        len: 1260,
        anchor: (11.98, 27.175),
    },
    Sprite {
        start: 151988,
        len: 1238,
        anchor: (11.615, 27.225),
    },
    Sprite {
        start: 153226,
        len: 1264,
        anchor: (11.565, 27.235),
    },
    Sprite {
        start: 154490,
        len: 1250,
        anchor: (11.875, 27.195),
    },
    Sprite {
        start: 155740,
        len: 1266,
        anchor: (12.335, 27.12),
    },
    Sprite {
        start: 157006,
        len: 1312,
        anchor: (12.675, 27.07),
    },
    Sprite {
        start: 158318,
        len: 1218,
        anchor: (12.71, 27.06),
    },
    Sprite {
        start: 159536,
        len: 1314,
        anchor: (12.435, 27.105),
    },
    Sprite {
        start: 160850,
        len: 1236,
        anchor: (12.465, 27.175),
    },
    Sprite {
        start: 162086,
        len: 1216,
        anchor: (11.985, 27.225),
    },
    Sprite {
        start: 163302,
        len: 1250,
        anchor: (11.65, 27.235),
    },
    Sprite {
        start: 164552,
        len: 1244,
        anchor: (12.485, 27.195),
    },
    Sprite {
        start: 165796,
        len: 1246,
        anchor: (12.335, 27.12),
    },
    Sprite {
        start: 167042,
        len: 578,
        anchor: (12.675, 27.07),
    },
    Sprite {
        start: 167620,
        len: 556,
        anchor: (12.71, 27.06),
    },
    Sprite {
        start: 168176,
        len: 544,
        anchor: (12.435, 27.105),
    },
    Sprite {
        start: 168720,
        len: 500,
        anchor: (11.98, 27.175),
    },
    Sprite {
        start: 169220,
        len: 534,
        anchor: (11.615, 27.225),
    },
    Sprite {
        start: 169754,
        len: 506,
        anchor: (11.565, 27.235),
    },
    Sprite {
        start: 170260,
        len: 530,
        anchor: (11.875, 27.195),
    },
    Sprite {
        start: 170790,
        len: 522,
        anchor: (12.335, 27.12),
    },
    Sprite {
        start: 171312,
        len: 590,
        anchor: (12.675, 27.07),
    },
    Sprite {
        start: 171902,
        len: 544,
        anchor: (12.71, 27.06),
    },
    Sprite {
        start: 172446,
        len: 528,
        anchor: (12.435, 27.105),
    },
    Sprite {
        start: 172974,
        len: 500,
        anchor: (12.465, 27.175),
    },
    Sprite {
        start: 173474,
        len: 510,
        anchor: (11.985, 27.225),
    },
    Sprite {
        start: 173984,
        len: 520,
        anchor: (11.65, 27.235),
    },
    Sprite {
        start: 174504,
        len: 468,
        anchor: (12.485, 27.195),
    },
    Sprite {
        start: 174972,
        len: 550,
        anchor: (12.335, 27.12),
    },
    Sprite {
        start: 175522,
        len: 906,
        anchor: (12.675, 26.335),
    },
    Sprite {
        start: 176428,
        len: 764,
        anchor: (12.71, 25.925),
    },
    Sprite {
        start: 177192,
        len: 804,
        anchor: (12.435, 25.475),
    },
    Sprite {
        start: 177996,
        len: 782,
        anchor: (11.98, 25.565),
    },
    Sprite {
        start: 178778,
        len: 746,
        anchor: (11.615, 25.56),
    },
    Sprite {
        start: 179524,
        len: 752,
        anchor: (11.565, 25.505),
    },
    Sprite {
        start: 180276,
        len: 778,
        anchor: (11.875, 25.435),
    },
    Sprite {
        start: 181054,
        len: 802,
        anchor: (12.335, 25.29),
    },
    Sprite {
        start: 181856,
        len: 758,
        anchor: (12.675, 25.56),
    },
    Sprite {
        start: 182614,
        len: 858,
        anchor: (12.71, 25.985),
    },
    Sprite {
        start: 183472,
        len: 864,
        anchor: (12.435, 26.375),
    },
    Sprite {
        start: 184336,
        len: 902,
        anchor: (12.465, 26.675),
    },
    Sprite {
        start: 185238,
        len: 836,
        anchor: (11.985, 26.88),
    },
    Sprite {
        start: 186074,
        len: 814,
        anchor: (11.65, 26.945),
    },
    Sprite {
        start: 186888,
        len: 926,
        anchor: (12.485, 26.87),
    },
    Sprite {
        start: 187814,
        len: 812,
        anchor: (12.335, 26.665),
    },
    Sprite {
        start: 188626,
        len: 830,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 189456,
        len: 788,
        anchor: (12.71, 23.16),
    },
    Sprite {
        start: 190244,
        len: 820,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 191064,
        len: 652,
        anchor: (11.98, 23.205),
    },
    Sprite {
        start: 191716,
        len: 542,
        anchor: (11.615, 23.22),
    },
    Sprite {
        start: 192258,
        len: 478,
        anchor: (11.565, 23.225),
    },
    Sprite {
        start: 192736,
        len: 426,
        anchor: (11.875, 23.21),
    },
    Sprite {
        start: 193162,
        len: 314,
        anchor: (12.335, 23.185),
    },
    Sprite {
        start: 193476,
        len: 94,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 193570,
        len: 94,
        anchor: (12.71, 23.16),
    },
    Sprite {
        start: 193664,
        len: 324,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 193988,
        len: 404,
        anchor: (12.465, 23.205),
    },
    Sprite {
        start: 194392,
        len: 432,
        anchor: (11.985, 23.22),
    },
    Sprite {
        start: 194824,
        len: 450,
        anchor: (11.65, 23.225),
    },
    Sprite {
        start: 195274,
        len: 606,
        anchor: (12.485, 23.21),
    },
    Sprite {
        start: 195880,
        len: 746,
        anchor: (12.335, 23.185),
    },
    Sprite {
        start: 196626,
        len: 1192,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 197818,
        len: 1146,
        anchor: (12.71, 23.16),
    },
    Sprite {
        start: 198964,
        len: 1142,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 200106,
        len: 1072,
        anchor: (11.98, 23.205),
    },
    Sprite {
        start: 201178,
        len: 960,
        anchor: (11.615, 23.22),
    },
    Sprite {
        start: 202138,
        len: 962,
        anchor: (11.565, 23.225),
    },
    Sprite {
        start: 203100,
        len: 902,
        anchor: (11.875, 23.21),
    },
    Sprite {
        start: 204002,
        len: 986,
        anchor: (12.335, 23.185),
    },
    Sprite {
        start: 204988,
        len: 986,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 205974,
        len: 954,
        anchor: (12.71, 23.16),
    },
    Sprite {
        start: 206928,
        len: 1000,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 207928,
        len: 956,
        anchor: (12.465, 23.205),
    },
    Sprite {
        start: 208884,
        len: 930,
        anchor: (11.985, 23.22),
    },
    Sprite {
        start: 209814,
        len: 940,
        anchor: (11.65, 23.225),
    },
    Sprite {
        start: 210754,
        len: 1042,
        anchor: (12.485, 23.21),
    },
    Sprite {
        start: 211796,
        len: 1118,
        anchor: (12.335, 23.185),
    },
    Sprite {
        start: 212914,
        len: 732,
        anchor: (14.605, 25.56),
    },
    Sprite {
        start: 213646,
        len: 752,
        anchor: (14.64, 25.57),
    },
    Sprite {
        start: 214398,
        len: 772,
        anchor: (14.605, 25.56),
    },
    Sprite {
        start: 215170,
        len: 748,
        anchor: (14.64, 25.57),
    },
    Sprite {
        start: 215918,
        len: 786,
        anchor: (14.605, 25.56),
    },
    Sprite {
        start: 216704,
        len: 816,
        anchor: (14.64, 25.57),
    },
    Sprite {
        start: 217520,
        len: 778,
        anchor: (14.605, 25.56),
    },
    Sprite {
        start: 218298,
        len: 766,
        anchor: (14.64, 25.57),
    },
    Sprite {
        start: 219064,
        len: 748,
        anchor: (14.605, 25.56),
    },
    Sprite {
        start: 219812,
        len: 748,
        anchor: (14.64, 25.57),
    },
    Sprite {
        start: 220560,
        len: 750,
        anchor: (14.605, 25.56),
    },
    Sprite {
        start: 221310,
        len: 760,
        anchor: (14.64, 25.57),
    },
    Sprite {
        start: 222070,
        len: 778,
        anchor: (14.605, 25.56),
    },
    Sprite {
        start: 222848,
        len: 794,
        anchor: (14.64, 25.57),
    },
    Sprite {
        start: 223642,
        len: 810,
        anchor: (14.605, 25.56),
    },
    Sprite {
        start: 224452,
        len: 778,
        anchor: (14.64, 25.57),
    },
    Sprite {
        start: 225230,
        len: 1434,
        anchor: (14.605, 25.56),
    },
    Sprite {
        start: 226664,
        len: 1462,
        anchor: (14.64, 25.57),
    },
    Sprite {
        start: 228126,
        len: 1464,
        anchor: (14.605, 25.56),
    },
    Sprite {
        start: 229590,
        len: 1456,
        anchor: (14.64, 25.57),
    },
    Sprite {
        start: 231046,
        len: 1436,
        anchor: (14.605, 25.56),
    },
    Sprite {
        start: 232482,
        len: 1482,
        anchor: (14.64, 25.57),
    },
    Sprite {
        start: 233964,
        len: 1458,
        anchor: (14.605, 25.56),
    },
    Sprite {
        start: 235422,
        len: 1498,
        anchor: (14.64, 25.57),
    },
    Sprite {
        start: 236920,
        len: 1464,
        anchor: (14.605, 25.56),
    },
    Sprite {
        start: 238384,
        len: 1424,
        anchor: (14.64, 25.57),
    },
    Sprite {
        start: 239808,
        len: 1466,
        anchor: (14.605, 25.56),
    },
    Sprite {
        start: 241274,
        len: 1458,
        anchor: (14.64, 25.57),
    },
    Sprite {
        start: 242732,
        len: 1454,
        anchor: (14.605, 25.56),
    },
    Sprite {
        start: 244186,
        len: 1476,
        anchor: (14.64, 25.57),
    },
    Sprite {
        start: 245662,
        len: 1462,
        anchor: (14.605, 25.56),
    },
    Sprite {
        start: 247124,
        len: 1456,
        anchor: (14.64, 25.57),
    },
    Sprite {
        start: 248580,
        len: 1150,
        anchor: (12.675, 23.595),
    },
    Sprite {
        start: 249730,
        len: 1058,
        anchor: (12.71, 23.59),
    },
    Sprite {
        start: 250788,
        len: 1044,
        anchor: (12.435, 23.61),
    },
    Sprite {
        start: 251832,
        len: 1016,
        anchor: (12.165, 23.635),
    },
    Sprite {
        start: 252848,
        len: 1032,
        anchor: (11.805, 23.65),
    },
    Sprite {
        start: 253880,
        len: 1096,
        anchor: (11.755, 23.655),
    },
    Sprite {
        start: 254976,
        len: 1138,
        anchor: (12.815, 23.64),
    },
    Sprite {
        start: 256114,
        len: 1158,
        anchor: (13.745, 23.615),
    },
    Sprite {
        start: 257272,
        len: 1154,
        anchor: (12.86, 23.595),
    },
    Sprite {
        start: 258426,
        len: 1204,
        anchor: (12.71, 23.59),
    },
    Sprite {
        start: 259630,
        len: 1140,
        anchor: (12.435, 23.61),
    },
    Sprite {
        start: 260770,
        len: 1166,
        anchor: (12.465, 23.635),
    },
    Sprite {
        start: 261936,
        len: 1098,
        anchor: (11.985, 23.65),
    },
    Sprite {
        start: 263034,
        len: 1116,
        anchor: (11.65, 23.655),
    },
    Sprite {
        start: 264150,
        len: 1110,
        anchor: (12.485, 23.64),
    },
    Sprite {
        start: 265260,
        len: 1128,
        anchor: (12.335, 23.615),
    },
    Sprite {
        start: 266388,
        len: 402,
        anchor: (12.675, 23.595),
    },
    Sprite {
        start: 266790,
        len: 376,
        anchor: (12.71, 23.59),
    },
    Sprite {
        start: 267166,
        len: 344,
        anchor: (12.435, 23.61),
    },
    Sprite {
        start: 267510,
        len: 320,
        anchor: (12.165, 23.635),
    },
    Sprite {
        start: 267830,
        len: 302,
        anchor: (11.805, 23.65),
    },
    Sprite {
        start: 268132,
        len: 208,
        anchor: (11.755, 23.655),
    },
    Sprite {
        start: 268340,
        len: 190,
        anchor: (12.815, 23.64),
    },
    Sprite {
        start: 268530,
        len: 94,
        anchor: (13.745, 23.615),
    },
    Sprite {
        start: 268624,
        len: 94,
        anchor: (12.86, 23.595),
    },
    Sprite {
        start: 268718,
        len: 94,
        anchor: (12.71, 23.59),
    },
    Sprite {
        start: 268812,
        len: 148,
        anchor: (12.435, 23.61),
    },
    Sprite {
        start: 268960,
        len: 198,
        anchor: (12.465, 23.635),
    },
    Sprite {
        start: 269158,
        len: 192,
        anchor: (11.985, 23.65),
    },
    Sprite {
        start: 269350,
        len: 244,
        anchor: (11.65, 23.655),
    },
    Sprite {
        start: 269594,
        len: 324,
        anchor: (12.485, 23.64),
    },
    Sprite {
        start: 269918,
        len: 396,
        anchor: (12.335, 23.615),
    },
    Sprite {
        start: 270314,
        len: 1270,
        anchor: (13.24, 24.29),
    },
    Sprite {
        start: 271584,
        len: 1308,
        anchor: (13.275, 24.3),
    },
    Sprite {
        start: 272892,
        len: 1248,
        anchor: (13.025, 24.215),
    },
    Sprite {
        start: 274140,
        len: 1190,
        anchor: (12.685, 24.16),
    },
    Sprite {
        start: 275330,
        len: 1186,
        anchor: (12.32, 24.18),
    },
    Sprite {
        start: 276516,
        len: 1270,
        anchor: (12.315, 24.18),
    },
    Sprite {
        start: 277786,
        len: 1426,
        anchor: (12.58, 24.165),
    },
    Sprite {
        start: 279212,
        len: 1564,
        anchor: (12.93, 24.185),
    },
    Sprite {
        start: 280776,
        len: 1600,
        anchor: (13.24, 24.29),
    },
    Sprite {
        start: 282376,
        len: 1610,
        anchor: (13.275, 24.3),
    },
    Sprite {
        start: 283986,
        len: 1598,
        anchor: (13.025, 24.215),
    },
    Sprite {
        start: 285584,
        len: 1500,
        anchor: (12.87, 24.16),
    },
    Sprite {
        start: 287084,
        len: 1448,
        anchor: (12.9, 24.18),
    },
    Sprite {
        start: 288532,
        len: 1258,
        anchor: (12.955, 24.18),
    },
    Sprite {
        start: 289790,
        len: 1218,
        anchor: (12.97, 24.165),
    },
    Sprite {
        start: 291008,
        len: 1238,
        anchor: (12.93, 24.185),
    },
    Sprite {
        start: 292246,
        len: 1626,
        anchor: (13.24, 24.29),
    },
    Sprite {
        start: 293872,
        len: 1674,
        anchor: (13.275, 24.3),
    },
    Sprite {
        start: 295546,
        len: 1612,
        anchor: (13.025, 24.215),
    },
    Sprite {
        start: 297158,
        len: 1610,
        anchor: (12.685, 24.16),
    },
    Sprite {
        start: 298768,
        len: 1588,
        anchor: (12.32, 24.18),
    },
    Sprite {
        start: 300356,
        len: 1576,
        anchor: (12.315, 24.18),
    },
    Sprite {
        start: 301932,
        len: 1604,
        anchor: (12.58, 24.165),
    },
    Sprite {
        start: 303536,
        len: 1532,
        anchor: (12.93, 24.185),
    },
    Sprite {
        start: 305068,
        len: 1426,
        anchor: (13.24, 24.29),
    },
    Sprite {
        start: 306494,
        len: 1384,
        anchor: (13.275, 24.3),
    },
    Sprite {
        start: 307878,
        len: 1516,
        anchor: (13.025, 24.215),
    },
    Sprite {
        start: 309394,
        len: 1600,
        anchor: (12.87, 24.16),
    },
    Sprite {
        start: 310994,
        len: 1604,
        anchor: (12.9, 24.18),
    },
    Sprite {
        start: 312598,
        len: 1582,
        anchor: (12.955, 24.18),
    },
    Sprite {
        start: 314180,
        len: 1502,
        anchor: (12.97, 24.165),
    },
    Sprite {
        start: 315682,
        len: 1540,
        anchor: (12.93, 24.185),
    },
    Sprite {
        start: 317222,
        len: 716,
        anchor: (12.675, 23.45),
    },
    Sprite {
        start: 317938,
        len: 786,
        anchor: (12.71, 23.45),
    },
    Sprite {
        start: 318724,
        len: 774,
        anchor: (12.435, 23.465),
    },
    Sprite {
        start: 319498,
        len: 792,
        anchor: (11.98, 23.49),
    },
    Sprite {
        start: 320290,
        len: 796,
        anchor: (11.615, 23.975),
    },
    Sprite {
        start: 321086,
        len: 870,
        anchor: (11.565, 24.725),
    },
    Sprite {
        start: 321956,
        len: 850,
        anchor: (11.875, 25.23),
    },
    Sprite {
        start: 322806,
        len: 846,
        anchor: (12.335, 25.47),
    },
    Sprite {
        start: 323652,
        len: 812,
        anchor: (12.675, 25.365),
    },
    Sprite {
        start: 324464,
        len: 878,
        anchor: (12.71, 24.985),
    },
    Sprite {
        start: 325342,
        len: 756,
        anchor: (12.435, 24.4),
    },
    Sprite {
        start: 326098,
        len: 746,
        anchor: (12.465, 23.665),
    },
    Sprite {
        start: 326844,
        len: 778,
        anchor: (11.985, 23.51),
    },
    Sprite {
        start: 327622,
        len: 744,
        anchor: (11.65, 23.51),
    },
    Sprite {
        start: 328366,
        len: 662,
        anchor: (12.485, 23.495),
    },
    Sprite {
        start: 329028,
        len: 714,
        anchor: (12.335, 23.47),
    },
    Sprite {
        start: 329742,
        len: 498,
        anchor: (12.675, 26.715),
    },
    Sprite {
        start: 330240,
        len: 520,
        anchor: (12.71, 26.65),
    },
    Sprite {
        start: 330760,
        len: 516,
        anchor: (12.435, 26.915),
    },
    Sprite {
        start: 331276,
        len: 460,
        anchor: (11.98, 27.145),
    },
    Sprite {
        start: 331736,
        len: 408,
        anchor: (11.615, 27.285),
    },
    Sprite {
        start: 332144,
        len: 402,
        anchor: (11.565, 27.3),
    },
    Sprite {
        start: 332546,
        len: 434,
        anchor: (11.875, 27.19),
    },
    Sprite {
        start: 332980,
        len: 504,
        anchor: (12.335, 26.975),
    },
    Sprite {
        start: 333484,
        len: 514,
        anchor: (12.675, 26.715),
    },
    Sprite {
        start: 333998,
        len: 514,
        anchor: (12.71, 26.65),
    },
    Sprite {
        start: 334512,
        len: 524,
        anchor: (12.435, 26.915),
    },
    Sprite {
        start: 335036,
        len: 446,
        anchor: (12.465, 27.145),
    },
    Sprite {
        start: 335482,
        len: 410,
        anchor: (11.985, 27.285),
    },
    Sprite {
        start: 335892,
        len: 410,
        anchor: (11.65, 27.3),
    },
    Sprite {
        start: 336302,
        len: 438,
        anchor: (12.485, 27.19),
    },
    Sprite {
        start: 336740,
        len: 510,
        anchor: (12.335, 26.975),
    },
    Sprite {
        start: 337250,
        len: 588,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 337838,
        len: 518,
        anchor: (12.71, 23.16),
    },
    Sprite {
        start: 338356,
        len: 476,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 338832,
        len: 396,
        anchor: (11.98, 23.205),
    },
    Sprite {
        start: 339228,
        len: 406,
        anchor: (11.615, 23.22),
    },
    Sprite {
        start: 339634,
        len: 304,
        anchor: (11.565, 23.225),
    },
    Sprite {
        start: 339938,
        len: 138,
        anchor: (11.875, 23.21),
    },
    Sprite {
        start: 340076,
        len: 178,
        anchor: (12.335, 23.185),
    },
    Sprite {
        start: 340254,
        len: 378,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 340632,
        len: 372,
        anchor: (12.71, 23.16),
    },
    Sprite {
        start: 341004,
        len: 448,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 341452,
        len: 428,
        anchor: (12.465, 23.205),
    },
    Sprite {
        start: 341880,
        len: 518,
        anchor: (11.985, 23.22),
    },
    Sprite {
        start: 342398,
        len: 592,
        anchor: (11.65, 23.225),
    },
    Sprite {
        start: 342990,
        len: 642,
        anchor: (12.485, 23.21),
    },
    Sprite {
        start: 343632,
        len: 608,
        anchor: (12.335, 23.185),
    },
    Sprite {
        start: 344240,
        len: 374,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 344614,
        len: 358,
        anchor: (12.71, 23.16),
    },
    Sprite {
        start: 344972,
        len: 318,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 345290,
        len: 288,
        anchor: (11.98, 23.205),
    },
    Sprite {
        start: 345578,
        len: 92,
        anchor: (11.615, 23.22),
    },
    Sprite {
        start: 345670,
        len: 92,
        anchor: (11.565, 23.225),
    },
    Sprite {
        start: 345762,
        len: 94,
        anchor: (11.875, 23.21),
    },
    Sprite {
        start: 345856,
        len: 94,
        anchor: (12.335, 23.185),
    },
    Sprite {
        start: 345950,
        len: 94,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 346044,
        len: 94,
        anchor: (12.71, 23.16),
    },
    Sprite {
        start: 346138,
        len: 302,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 346440,
        len: 342,
        anchor: (12.465, 23.205),
    },
    Sprite {
        start: 346782,
        len: 388,
        anchor: (11.985, 23.22),
    },
    Sprite {
        start: 347170,
        len: 378,
        anchor: (11.65, 23.225),
    },
    Sprite {
        start: 347548,
        len: 390,
        anchor: (12.485, 23.21),
    },
    Sprite {
        start: 347938,
        len: 404,
        anchor: (12.335, 23.185),
    },
    Sprite {
        start: 348342,
        len: 682,
        anchor: (15.46, 23.53),
    },
    Sprite {
        start: 349024,
        len: 722,
        anchor: (15.625, 23.5),
    },
    Sprite {
        start: 349746,
        len: 742,
        anchor: (14.475, 23.635),
    },
    Sprite {
        start: 350488,
        len: 482,
        anchor: (11.98, 23.875),
    },
    Sprite {
        start: 350970,
        len: 538,
        anchor: (11.615, 24.055),
    },
    Sprite {
        start: 351508,
        len: 560,
        anchor: (11.565, 24.075),
    },
    Sprite {
        start: 352068,
        len: 548,
        anchor: (11.875, 23.92),
    },
    Sprite {
        start: 352616,
        len: 676,
        anchor: (13.905, 23.705),
    },
    Sprite {
        start: 353292,
        len: 672,
        anchor: (15.46, 23.53),
    },
    Sprite {
        start: 353964,
        len: 726,
        anchor: (15.625, 23.5),
    },
    Sprite {
        start: 354690,
        len: 738,
        anchor: (14.475, 23.635),
    },
    Sprite {
        start: 355428,
        len: 568,
        anchor: (12.465, 23.875),
    },
    Sprite {
        start: 355996,
        len: 520,
        anchor: (11.985, 24.055),
    },
    Sprite {
        start: 356516,
        len: 500,
        anchor: (11.65, 24.075),
    },
    Sprite {
        start: 357016,
        len: 556,
        anchor: (12.485, 23.92),
    },
    Sprite {
        start: 357572,
        len: 714,
        anchor: (13.905, 23.705),
    },
    Sprite {
        start: 358286,
        len: 1378,
        anchor: (15.46, 23.53),
    },
    Sprite {
        start: 359664,
        len: 1432,
        anchor: (15.625, 23.5),
    },
    Sprite {
        start: 361096,
        len: 1390,
        anchor: (14.475, 23.635),
    },
    Sprite {
        start: 362486,
        len: 1156,
        anchor: (11.98, 23.875),
    },
    Sprite {
        start: 363642,
        len: 900,
        anchor: (11.615, 24.055),
    },
    Sprite {
        start: 364542,
        len: 794,
        anchor: (11.565, 24.075),
    },
    Sprite {
        start: 365336,
        len: 1054,
        anchor: (11.875, 23.92),
    },
    Sprite {
        start: 366390,
        len: 1416,
        anchor: (13.905, 23.705),
    },
    Sprite {
        start: 367806,
        len: 1406,
        anchor: (15.46, 23.53),
    },
    Sprite {
        start: 369212,
        len: 1388,
        anchor: (15.625, 23.5),
    },
    Sprite {
        start: 370600,
        len: 1422,
        anchor: (14.475, 23.635),
    },
    Sprite {
        start: 372022,
        len: 1154,
        anchor: (12.465, 23.875),
    },
    Sprite {
        start: 373176,
        len: 954,
        anchor: (11.985, 24.055),
    },
    Sprite {
        start: 374130,
        len: 834,
        anchor: (11.65, 24.075),
    },
    Sprite {
        start: 374964,
        len: 1066,
        anchor: (12.485, 23.92),
    },
    Sprite {
        start: 376030,
        len: 1378,
        anchor: (13.905, 23.705),
    },
    Sprite {
        start: 377408,
        len: 292,
        anchor: (12.675, 28.935),
    },
    Sprite {
        start: 377700,
        len: 290,
        anchor: (12.71, 29.145),
    },
    Sprite {
        start: 377990,
        len: 272,
        anchor: (12.435, 29.33),
    },
    Sprite {
        start: 378262,
        len: 260,
        anchor: (11.98, 29.47),
    },
    Sprite {
        start: 378522,
        len: 274,
        anchor: (11.615, 29.535),
    },
    Sprite {
        start: 378796,
        len: 254,
        anchor: (11.565, 29.52),
    },
    Sprite {
        start: 379050,
        len: 252,
        anchor: (11.875, 29.425),
    },
    Sprite {
        start: 379302,
        len: 268,
        anchor: (12.335, 29.265),
    },
    Sprite {
        start: 379570,
        len: 246,
        anchor: (12.675, 29.065),
    },
    Sprite {
        start: 379816,
        len: 242,
        anchor: (12.71, 28.855),
    },
    Sprite {
        start: 380058,
        len: 272,
        anchor: (12.435, 28.665),
    },
    Sprite {
        start: 380330,
        len: 266,
        anchor: (12.465, 28.53),
    },
    Sprite {
        start: 380596,
        len: 250,
        anchor: (11.985, 28.465),
    },
    Sprite {
        start: 380846,
        len: 236,
        anchor: (11.65, 28.48),
    },
    Sprite {
        start: 381082,
        len: 256,
        anchor: (12.485, 28.575),
    },
    Sprite {
        start: 381338,
        len: 278,
        anchor: (12.335, 28.735),
    },
    Sprite {
        start: 381616,
        len: 318,
        anchor: (12.675, 28.935),
    },
    Sprite {
        start: 381934,
        len: 344,
        anchor: (12.71, 29.145),
    },
    Sprite {
        start: 382278,
        len: 374,
        anchor: (12.435, 29.33),
    },
    Sprite {
        start: 382652,
        len: 302,
        anchor: (11.98, 29.47),
    },
    Sprite {
        start: 382954,
        len: 346,
        anchor: (11.615, 29.535),
    },
    Sprite {
        start: 383300,
        len: 236,
        anchor: (11.565, 29.52),
    },
    Sprite {
        start: 383536,
        len: 278,
        anchor: (11.875, 29.425),
    },
    Sprite {
        start: 383814,
        len: 300,
        anchor: (12.335, 29.265),
    },
    Sprite {
        start: 384114,
        len: 336,
        anchor: (12.675, 29.065),
    },
    Sprite {
        start: 384450,
        len: 346,
        anchor: (12.71, 28.855),
    },
    Sprite {
        start: 384796,
        len: 292,
        anchor: (12.435, 28.665),
    },
    Sprite {
        start: 385088,
        len: 304,
        anchor: (12.465, 28.53),
    },
    Sprite {
        start: 385392,
        len: 266,
        anchor: (11.985, 28.465),
    },
    Sprite {
        start: 385658,
        len: 260,
        anchor: (11.65, 28.48),
    },
    Sprite {
        start: 385918,
        len: 294,
        anchor: (12.485, 28.575),
    },
    Sprite {
        start: 386212,
        len: 318,
        anchor: (12.335, 28.735),
    },
    Sprite {
        start: 386530,
        len: 804,
        anchor: (12.675, 29.32),
    },
    Sprite {
        start: 387334,
        len: 710,
        anchor: (12.71, 29.33),
    },
    Sprite {
        start: 388044,
        len: 638,
        anchor: (12.435, 29.26),
    },
    Sprite {
        start: 388682,
        len: 642,
        anchor: (11.98, 29.5),
    },
    Sprite {
        start: 389324,
        len: 710,
        anchor: (11.615, 29.645),
    },
    Sprite {
        start: 390034,
        len: 616,
        anchor: (11.565, 29.66),
    },
    Sprite {
        start: 390650,
        len: 630,
        anchor: (11.875, 29.55),
    },
    Sprite {
        start: 391280,
        len: 660,
        anchor: (12.335, 29.32),
    },
    Sprite {
        start: 391940,
        len: 792,
        anchor: (12.675, 29.32),
    },
    Sprite {
        start: 392732,
        len: 710,
        anchor: (12.71, 29.33),
    },
    Sprite {
        start: 393442,
        len: 680,
        anchor: (12.435, 29.26),
    },
    Sprite {
        start: 394122,
        len: 586,
        anchor: (12.465, 29.5),
    },
    Sprite {
        start: 394708,
        len: 616,
        anchor: (11.985, 29.645),
    },
    Sprite {
        start: 395324,
        len: 734,
        anchor: (11.65, 29.66),
    },
    Sprite {
        start: 396058,
        len: 646,
        anchor: (12.485, 29.55),
    },
    Sprite {
        start: 396704,
        len: 632,
        anchor: (12.335, 29.32),
    },
    Sprite {
        start: 397336,
        len: 580,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 397916,
        len: 496,
        anchor: (12.71, 23.16),
    },
    Sprite {
        start: 398412,
        len: 422,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 398834,
        len: 346,
        anchor: (11.98, 23.205),
    },
    Sprite {
        start: 399180,
        len: 368,
        anchor: (11.615, 23.22),
    },
    Sprite {
        start: 399548,
        len: 298,
        anchor: (11.565, 23.225),
    },
    Sprite {
        start: 399846,
        len: 94,
        anchor: (11.875, 23.21),
    },
    Sprite {
        start: 399940,
        len: 94,
        anchor: (12.335, 23.185),
    },
    Sprite {
        start: 400034,
        len: 326,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 400360,
        len: 362,
        anchor: (12.71, 23.16),
    },
    Sprite {
        start: 400722,
        len: 400,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 401122,
        len: 380,
        anchor: (12.465, 23.205),
    },
    Sprite {
        start: 401502,
        len: 420,
        anchor: (11.985, 23.22),
    },
    Sprite {
        start: 401922,
        len: 500,
        anchor: (11.65, 23.225),
    },
    Sprite {
        start: 402422,
        len: 532,
        anchor: (12.485, 23.21),
    },
    Sprite {
        start: 402954,
        len: 492,
        anchor: (12.335, 23.185),
    },
    Sprite {
        start: 403446,
        len: 1096,
        anchor: (12.85, 23.165),
    },
    Sprite {
        start: 404542,
        len: 1122,
        anchor: (13.02, 23.16),
    },
    Sprite {
        start: 405664,
        len: 1030,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 406694,
        len: 916,
        anchor: (11.98, 23.205),
    },
    Sprite {
        start: 407610,
        len: 716,
        anchor: (11.615, 23.22),
    },
    Sprite {
        start: 408326,
        len: 626,
        anchor: (11.565, 23.225),
    },
    Sprite {
        start: 408952,
        len: 478,
        anchor: (11.875, 23.21),
    },
    Sprite {
        start: 409430,
        len: 380,
        anchor: (12.335, 23.185),
    },
    Sprite {
        start: 409810,
        len: 288,
        anchor: (12.775, 23.165),
    },
    Sprite {
        start: 410098,
        len: 280,
        anchor: (13.07, 23.16),
    },
    Sprite {
        start: 410378,
        len: 242,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 410620,
        len: 438,
        anchor: (13.6, 23.205),
    },
    Sprite {
        start: 411058,
        len: 472,
        anchor: (12.99, 23.22),
    },
    Sprite {
        start: 411530,
        len: 616,
        anchor: (13.025, 23.225),
    },
    Sprite {
        start: 412146,
        len: 882,
        anchor: (13.63, 23.21),
    },
    Sprite {
        start: 413028,
        len: 976,
        anchor: (12.76, 23.185),
    },
    Sprite {
        start: 414004,
        len: 1082,
        anchor: (12.85, 23.165),
    },
    Sprite {
        start: 415086,
        len: 1132,
        anchor: (13.02, 23.16),
    },
    Sprite {
        start: 416218,
        len: 1052,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 417270,
        len: 906,
        anchor: (11.98, 23.205),
    },
    Sprite {
        start: 418176,
        len: 718,
        anchor: (11.615, 23.22),
    },
    Sprite {
        start: 418894,
        len: 594,
        anchor: (11.565, 23.225),
    },
    Sprite {
        start: 419488,
        len: 470,
        anchor: (11.875, 23.21),
    },
    Sprite {
        start: 419958,
        len: 380,
        anchor: (12.335, 23.185),
    },
    Sprite {
        start: 420338,
        len: 288,
        anchor: (12.775, 23.165),
    },
    Sprite {
        start: 420626,
        len: 280,
        anchor: (13.07, 23.16),
    },
    Sprite {
        start: 420906,
        len: 242,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 421148,
        len: 448,
        anchor: (13.6, 23.205),
    },
    Sprite {
        start: 421596,
        len: 472,
        anchor: (12.99, 23.22),
    },
    Sprite {
        start: 422068,
        len: 642,
        anchor: (13.025, 23.225),
    },
    Sprite {
        start: 422710,
        len: 854,
        anchor: (13.63, 23.21),
    },
    Sprite {
        start: 423564,
        len: 972,
        anchor: (12.76, 23.185),
    },
    Sprite {
        start: 424536,
        len: 508,
        anchor: (12.85, 23.165),
    },
    Sprite {
        start: 425044,
        len: 558,
        anchor: (13.02, 23.16),
    },
    Sprite {
        start: 425602,
        len: 580,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 426182,
        len: 566,
        anchor: (11.98, 23.205),
    },
    Sprite {
        start: 426748,
        len: 388,
        anchor: (11.615, 23.22),
    },
    Sprite {
        start: 427136,
        len: 384,
        anchor: (11.565, 23.225),
    },
    Sprite {
        start: 427520,
        len: 248,
        anchor: (11.875, 23.21),
    },
    Sprite {
        start: 427768,
        len: 94,
        anchor: (12.335, 23.185),
    },
    Sprite {
        start: 427862,
        len: 94,
        anchor: (12.775, 23.165),
    },
    Sprite {
        start: 427956,
        len: 94,
        anchor: (13.07, 23.16),
    },
    Sprite {
        start: 428050,
        len: 94,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 428144,
        len: 200,
        anchor: (13.6, 23.205),
    },
    Sprite {
        start: 428344,
        len: 128,
        anchor: (12.99, 23.22),
    },
    Sprite {
        start: 428472,
        len: 372,
        anchor: (13.025, 23.225),
    },
    Sprite {
        start: 428844,
        len: 444,
        anchor: (13.63, 23.21),
    },
    Sprite {
        start: 429288,
        len: 536,
        anchor: (12.76, 23.185),
    },
    Sprite {
        start: 429824,
        len: 442,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 430266,
        len: 448,
        anchor: (12.71, 23.16),
    },
    Sprite {
        start: 430714,
        len: 468,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 431182,
        len: 468,
        anchor: (11.98, 23.205),
    },
    Sprite {
        start: 431650,
        len: 370,
        anchor: (11.615, 23.22),
    },
    Sprite {
        start: 432020,
        len: 300,
        anchor: (11.565, 23.225),
    },
    Sprite {
        start: 432320,
        len: 280,
        anchor: (11.875, 23.21),
    },
    Sprite {
        start: 432600,
        len: 94,
        anchor: (12.335, 23.185),
    },
    Sprite {
        start: 432694,
        len: 94,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 432788,
        len: 94,
        anchor: (12.71, 23.16),
    },
    Sprite {
        start: 432882,
        len: 94,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 432976,
        len: 186,
        anchor: (12.465, 23.205),
    },
    Sprite {
        start: 433162,
        len: 272,
        anchor: (12.7, 23.22),
    },
    Sprite {
        start: 433434,
        len: 354,
        anchor: (12.6, 23.225),
    },
    Sprite {
        start: 433788,
        len: 412,
        anchor: (12.485, 23.21),
    },
    Sprite {
        start: 434200,
        len: 466,
        anchor: (12.335, 23.185),
    },
    Sprite {
        start: 434666,
        len: 964,
        anchor: (12.92, 23.165),
    },
    Sprite {
        start: 435630,
        len: 954,
        anchor: (12.96, 23.16),
    },
    Sprite {
        start: 436584,
        len: 930,
        anchor: (12.7, 23.18),
    },
    Sprite {
        start: 437514,
        len: 952,
        anchor: (12.265, 23.205),
    },
    Sprite {
        start: 438466,
        len: 914,
        anchor: (11.92, 23.22),
    },
    Sprite {
        start: 439380,
        len: 902,
        anchor: (11.87, 23.225),
    },
    Sprite {
        start: 440282,
        len: 930,
        anchor: (12.165, 23.21),
    },
    Sprite {
        start: 441212,
        len: 860,
        anchor: (12.6, 23.185),
    },
    Sprite {
        start: 442072,
        len: 848,
        anchor: (12.92, 23.165),
    },
    Sprite {
        start: 442920,
        len: 874,
        anchor: (12.96, 23.16),
    },
    Sprite {
        start: 443794,
        len: 884,
        anchor: (12.7, 23.18),
    },
    Sprite {
        start: 444678,
        len: 940,
        anchor: (12.465, 23.205),
    },
    Sprite {
        start: 445618,
        len: 998,
        anchor: (12.265, 23.22),
    },
    Sprite {
        start: 446616,
        len: 984,
        anchor: (11.87, 23.225),
    },
    Sprite {
        start: 447600,
        len: 1062,
        anchor: (12.485, 23.21),
    },
    Sprite {
        start: 448662,
        len: 962,
        anchor: (12.6, 23.185),
    },
    Sprite {
        start: 449624,
        len: 706,
        anchor: (5.485, 7.68),
    },
    Sprite {
        start: 450330,
        len: 734,
        anchor: (5.485, 7.68),
    },
    Sprite {
        start: 451064,
        len: 744,
        anchor: (5.485, 7.68),
    },
    Sprite {
        start: 451808,
        len: 762,
        anchor: (5.485, 7.68),
    },
    Sprite {
        start: 452570,
        len: 790,
        anchor: (5.485, 7.68),
    },
    Sprite {
        start: 453360,
        len: 402,
        anchor: (3.665, 4.61),
    },
    Sprite {
        start: 453762,
        len: 416,
        anchor: (3.665, 4.61),
    },
    Sprite {
        start: 454178,
        len: 416,
        anchor: (3.665, 4.61),
    },
    Sprite {
        start: 454594,
        len: 430,
        anchor: (3.665, 4.61),
    },
    Sprite {
        start: 455024,
        len: 434,
        anchor: (3.665, 4.61),
    },
    Sprite {
        start: 455458,
        len: 172,
        anchor: (1.44, 1.44),
    },
    Sprite {
        start: 455630,
        len: 252,
        anchor: (2.16, 2.16),
    },
    Sprite {
        start: 455882,
        len: 214,
        anchor: (1.56, 1.895),
    },
    Sprite {
        start: 456096,
        len: 406,
        anchor: (1.955, 5.54),
    },
    Sprite {
        start: 456502,
        len: 296,
        anchor: (2.64, 2.415),
    },
    Sprite {
        start: 456798,
        len: 240,
        anchor: (1.8, 1.8),
    },
    Sprite {
        start: 457038,
        len: 230,
        anchor: (1.8, 1.8),
    },
    Sprite {
        start: 457268,
        len: 382,
        anchor: (1.8, 4.475),
    },
    Sprite {
        start: 457650,
        len: 342,
        anchor: (2.875, 2.955),
    },
    Sprite {
        start: 457992,
        len: 350,
        anchor: (2.95, 2.77),
    },
    Sprite {
        start: 458342,
        len: 364,
        anchor: (2.88, 4.025),
    },
    Sprite {
        start: 458706,
        len: 364,
        anchor: (2.88, 4.025),
    },
    Sprite {
        start: 459070,
        len: 298,
        anchor: (1.145, 4.41),
    },
    Sprite {
        start: 459368,
        len: 244,
        anchor: (1.7, 1.635),
    },
    Sprite {
        start: 459612,
        len: 252,
        anchor: (1.58, 1.665),
    },
    Sprite {
        start: 459864,
        len: 210,
        anchor: (1.29, 1.615),
    },
    Sprite {
        start: 460074,
        len: 196,
        anchor: (1.445, 1.69),
    },
    Sprite {
        start: 460270,
        len: 294,
        anchor: (2.415, 2.79),
    },
    Sprite {
        start: 460564,
        len: 320,
        anchor: (2.815, 4.325),
    },
    Sprite {
        start: 460884,
        len: 374,
        anchor: (3.21, 5.865),
    },
    Sprite {
        start: 461258,
        len: 418,
        anchor: (3.61, 7.405),
    },
    Sprite {
        start: 461676,
        len: 644,
        anchor: (4.015, 9.905),
    },
    Sprite {
        start: 462320,
        len: 1806,
        anchor: (17.48, 17.48),
    },
    Sprite {
        start: 464126,
        len: 1720,
        anchor: (17.48, 17.48),
    },
    Sprite {
        start: 465846,
        len: 1580,
        anchor: (17.48, 17.48),
    },
    Sprite {
        start: 467426,
        len: 1588,
        anchor: (17.48, 17.48),
    },
    Sprite {
        start: 469014,
        len: 1614,
        anchor: (17.48, 17.48),
    },
    Sprite {
        start: 470628,
        len: 1720,
        anchor: (17.48, 17.48),
    },
    Sprite {
        start: 472348,
        len: 1716,
        anchor: (17.48, 17.48),
    },
    Sprite {
        start: 474064,
        len: 3126,
        anchor: (24.145, 24.145),
    },
    Sprite {
        start: 477190,
        len: 3142,
        anchor: (24.145, 24.145),
    },
    Sprite {
        start: 480332,
        len: 2946,
        anchor: (24.145, 24.145),
    },
    Sprite {
        start: 483278,
        len: 2874,
        anchor: (24.145, 24.145),
    },
    Sprite {
        start: 486152,
        len: 3114,
        anchor: (24.145, 24.145),
    },
    Sprite {
        start: 489266,
        len: 3088,
        anchor: (24.145, 24.145),
    },
    Sprite {
        start: 492354,
        len: 3116,
        anchor: (24.145, 24.145),
    },
    Sprite {
        start: 495470,
        len: 632,
        anchor: (4.27, 4.27),
    },
    Sprite {
        start: 496102,
        len: 664,
        anchor: (4.27, 4.27),
    },
    Sprite {
        start: 496766,
        len: 678,
        anchor: (4.27, 4.27),
    },
    Sprite {
        start: 497444,
        len: 688,
        anchor: (4.27, 4.27),
    },
    Sprite {
        start: 498132,
        len: 658,
        anchor: (4.27, 4.27),
    },
    Sprite {
        start: 498790,
        len: 646,
        anchor: (4.27, 4.27),
    },
    Sprite {
        start: 499436,
        len: 676,
        anchor: (4.27, 4.27),
    },
    Sprite {
        start: 500112,
        len: 636,
        anchor: (4.27, 4.27),
    },
    Sprite {
        start: 500748,
        len: 282,
        anchor: (11.0, 3.5),
    },
];

// 由 scripts/buddies_pack.py 生成，别手改：改 buddies_render.py 重渲再打包。

use super::{Sprite, Wearable};

pub(super) const BLOB: &[u8] = include_bytes!("../../assets/buddies.bin");

/// 会转的零件每组几个关键朝向（每 45° 一张，从 0° 起）。
pub(super) const KEYS: usize = 8;
/// 跟着身子转身的零件每组几个朝向（每 45° 一张，0 是正面，往画面右边转为正）。
pub(super) const FACINGS: usize = 8;
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
pub(super) const BLUSH: usize = 8;
pub(super) const ARM: usize = 16;
pub(super) const PROP_WRENCH: usize = 24;
pub(super) const PROP_MAGNIFIER: usize = 32;
pub(super) const PROP_PENCIL: usize = 40;
pub(super) const PROP_FLASK: usize = 48;
pub(super) const PROP_CAN: usize = 56;
pub(super) const PROP_BATON: usize = 64;
pub(super) const PROP_BROOM: usize = 72;
pub(super) const EYE_OPEN_L: usize = 80;
pub(super) const EYE_OPEN_R: usize = 88;
pub(super) const EYE_HAPPY_L: usize = 96;
pub(super) const EYE_HAPPY_R: usize = 104;
pub(super) const EYE_SHUT_L: usize = 112;
pub(super) const EYE_SHUT_R: usize = 120;
pub(super) const MOUTH_SMILE: usize = 128;
pub(super) const MOUTH_FLAT: usize = 136;
pub(super) const MOUTH_OPEN: usize = 144;
pub(super) const HEAD_BEANIE_BASE: usize = 152;
pub(super) const HEAD_BEANIE_TRIM: usize = 160;
pub(super) const HEAD_CAP_BASE: usize = 168;
pub(super) const HEAD_FEDORA_BASE: usize = 176;
pub(super) const HEAD_FEDORA_TRIM: usize = 184;
pub(super) const HEAD_BERET_BASE: usize = 192;
pub(super) const HEAD_GOGGLES_BASE: usize = 200;
pub(super) const HEAD_GOGGLES_TRIM: usize = 208;
pub(super) const HEAD_STRAW_BASE: usize = 216;
pub(super) const HEAD_STRAW_TRIM: usize = 224;
pub(super) const HEAD_BANDANA_BASE: usize = 232;
pub(super) const HEAD_BANDANA_TRIM: usize = 240;
pub(super) const HEAD_HELMET_BASE: usize = 248;
pub(super) const HEAD_HELMET_TRIM: usize = 256;
pub(super) const HEAD_SLICK_TRIM: usize = 264;
pub(super) const HEAD_SPRIG_TRIM: usize = 272;
pub(super) const HEAD_FLOWER_BASE: usize = 280;
pub(super) const HEAD_FLOWER_TRIM: usize = 288;
pub(super) const HEAD_HEADPHONES_BASE: usize = 296;
pub(super) const HEAD_HEADPHONES_TRIM: usize = 304;
pub(super) const HEAD_ANTENNA_BASE: usize = 312;
pub(super) const HEAD_ANTENNA_TRIM: usize = 320;
pub(super) const HEAD_CHEF_TRIM: usize = 328;
pub(super) const HEAD_BOW_BASE: usize = 336;
pub(super) const FACE_GLASSES_BASE: usize = 344;
pub(super) const FACE_SUNGLASSES_BASE: usize = 352;
pub(super) const FACE_SUNGLASSES_TRIM: usize = 360;
pub(super) const NECK_BOW_TIE_BASE: usize = 368;
pub(super) const NECK_SCARF_BASE: usize = 376;
pub(super) const LAPTOP: usize = 384;
pub(super) const NOTEPAD: usize = 389;
pub(super) const FX_SPARK: usize = 394;
pub(super) const FX_BUBBLE: usize = 395;
pub(super) const FX_DROP: usize = 396;
pub(super) const FX_NOTE: usize = 397;
pub(super) const FX_PUFF: usize = 398;
pub(super) const FX_DOT: usize = 399;
pub(super) const FX_DOT_LIT: usize = 400;
pub(super) const FX_BANG: usize = 401;
pub(super) const FX_STAR: usize = 402;
pub(super) const FX_HEART: usize = 403;
pub(super) const FX_BULB: usize = 404;
pub(super) const FX_BULB_LIT: usize = 405;
pub(super) const FX_SPEED: usize = 406;
pub(super) const CONFETTI: usize = 407;
pub(super) const SPROUT: usize = 411;
pub(super) const KART_FRONT: usize = 416;
pub(super) const KART_BACK: usize = 423;
pub(super) const WHEEL: usize = 430;
pub(super) const SHADOW: usize = 438;

/// 拼装用的位置（pt）。`FEET` 是脚底在画布上的位置；身上各处相对脚底；`TIP_*` 是道具
/// 朝上时尖端相对握点；`NOTEPAD_AT` 相对左手；`ARM_LEN` 是肩到手。`SHOULDERS` 是每个朝向
/// 左、右肩的位置和比身子中轴离镜头远多少（正的在身子后面）。
pub(super) const CANVAS: f32 = 36.0;
pub(super) const FEET: (f32, f32) = (18.0, 28.605);
pub(super) const SHOULDERS: [[(f32, f32, f32); 2]; 8] = [
    [(-11.06, -9.76, 0.275), (9.9, -8.305, -4.745)],
    [(-8.02, -7.56, -7.32), (10.495, -10.655, 3.365)],
    [(-0.285, -6.6, -10.62), (4.94, -12.44, 9.505)],
    [(7.62, -7.445, -7.705), (-3.505, -12.605, 10.08)],
    [(11.06, -9.6, -0.275), (-9.9, -11.055, 4.745)],
    [(8.02, -11.805, 7.32), (-10.495, -8.705, -3.365)],
    [(0.285, -12.76, 10.62), (-4.94, -6.925, -9.505)],
    [(-7.62, -11.915, 7.705), (3.505, -6.76, -10.08)],
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

pub(super) const SPRITES: [Sprite; 439] = [
    Sprite {
        start: 0,
        len: 1594,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 1594,
        len: 1526,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 3120,
        len: 1482,
        anchor: (11.615, 23.22),
    },
    Sprite {
        start: 4602,
        len: 1532,
        anchor: (11.875, 23.21),
    },
    Sprite {
        start: 6134,
        len: 1310,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 7444,
        len: 1390,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 8834,
        len: 1342,
        anchor: (11.985, 23.22),
    },
    Sprite {
        start: 10176,
        len: 1536,
        anchor: (12.485, 23.21),
    },
    Sprite {
        start: 11712,
        len: 538,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 12250,
        len: 562,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 12812,
        len: 310,
        anchor: (11.615, 23.22),
    },
    Sprite {
        start: 13122,
        len: 282,
        anchor: (11.875, 23.21),
    },
    Sprite {
        start: 13404,
        len: 94,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 13498,
        len: 94,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 13592,
        len: 272,
        anchor: (11.985, 23.22),
    },
    Sprite {
        start: 13864,
        len: 420,
        anchor: (12.485, 23.21),
    },
    Sprite {
        start: 14284,
        len: 536,
        anchor: (6.72, 6.72),
    },
    Sprite {
        start: 14820,
        len: 462,
        anchor: (6.72, 6.72),
    },
    Sprite {
        start: 15282,
        len: 538,
        anchor: (6.72, 6.72),
    },
    Sprite {
        start: 15820,
        len: 500,
        anchor: (6.72, 6.72),
    },
    Sprite {
        start: 16320,
        len: 524,
        anchor: (6.72, 6.72),
    },
    Sprite {
        start: 16844,
        len: 496,
        anchor: (6.72, 6.72),
    },
    Sprite {
        start: 17340,
        len: 504,
        anchor: (6.72, 6.72),
    },
    Sprite {
        start: 17844,
        len: 486,
        anchor: (6.72, 6.72),
    },
    Sprite {
        start: 18330,
        len: 888,
        anchor: (14.555, 14.555),
    },
    Sprite {
        start: 19218,
        len: 1002,
        anchor: (14.555, 14.555),
    },
    Sprite {
        start: 20220,
        len: 876,
        anchor: (14.555, 14.555),
    },
    Sprite {
        start: 21096,
        len: 1054,
        anchor: (14.555, 14.555),
    },
    Sprite {
        start: 22150,
        len: 938,
        anchor: (14.555, 14.555),
    },
    Sprite {
        start: 23088,
        len: 1054,
        anchor: (14.555, 14.555),
    },
    Sprite {
        start: 24142,
        len: 824,
        anchor: (14.555, 14.555),
    },
    Sprite {
        start: 24966,
        len: 1020,
        anchor: (14.555, 14.555),
    },
    Sprite {
        start: 25986,
        len: 958,
        anchor: (14.615, 14.615),
    },
    Sprite {
        start: 26944,
        len: 1106,
        anchor: (14.615, 14.615),
    },
    Sprite {
        start: 28050,
        len: 1010,
        anchor: (14.615, 14.615),
    },
    Sprite {
        start: 29060,
        len: 1094,
        anchor: (14.615, 14.615),
    },
    Sprite {
        start: 30154,
        len: 1026,
        anchor: (14.615, 14.615),
    },
    Sprite {
        start: 31180,
        len: 1088,
        anchor: (14.615, 14.615),
    },
    Sprite {
        start: 32268,
        len: 944,
        anchor: (14.615, 14.615),
    },
    Sprite {
        start: 33212,
        len: 1094,
        anchor: (14.615, 14.615),
    },
    Sprite {
        start: 34306,
        len: 734,
        anchor: (10.055, 10.055),
    },
    Sprite {
        start: 35040,
        len: 740,
        anchor: (10.055, 10.055),
    },
    Sprite {
        start: 35780,
        len: 730,
        anchor: (10.055, 10.055),
    },
    Sprite {
        start: 36510,
        len: 664,
        anchor: (10.055, 10.055),
    },
    Sprite {
        start: 37174,
        len: 754,
        anchor: (10.055, 10.055),
    },
    Sprite {
        start: 37928,
        len: 718,
        anchor: (10.055, 10.055),
    },
    Sprite {
        start: 38646,
        len: 680,
        anchor: (10.055, 10.055),
    },
    Sprite {
        start: 39326,
        len: 632,
        anchor: (10.055, 10.055),
    },
    Sprite {
        start: 39958,
        len: 766,
        anchor: (11.22, 11.22),
    },
    Sprite {
        start: 40724,
        len: 778,
        anchor: (11.22, 11.22),
    },
    Sprite {
        start: 41502,
        len: 712,
        anchor: (11.22, 11.22),
    },
    Sprite {
        start: 42214,
        len: 774,
        anchor: (11.22, 11.22),
    },
    Sprite {
        start: 42988,
        len: 736,
        anchor: (11.22, 11.22),
    },
    Sprite {
        start: 43724,
        len: 744,
        anchor: (11.22, 11.22),
    },
    Sprite {
        start: 44468,
        len: 812,
        anchor: (11.22, 11.22),
    },
    Sprite {
        start: 45280,
        len: 808,
        anchor: (11.22, 11.22),
    },
    Sprite {
        start: 46088,
        len: 998,
        anchor: (9.045, 9.045),
    },
    Sprite {
        start: 47086,
        len: 970,
        anchor: (9.045, 9.045),
    },
    Sprite {
        start: 48056,
        len: 910,
        anchor: (9.045, 9.045),
    },
    Sprite {
        start: 48966,
        len: 996,
        anchor: (9.045, 9.045),
    },
    Sprite {
        start: 49962,
        len: 964,
        anchor: (9.045, 9.045),
    },
    Sprite {
        start: 50926,
        len: 964,
        anchor: (9.045, 9.045),
    },
    Sprite {
        start: 51890,
        len: 1022,
        anchor: (9.045, 9.045),
    },
    Sprite {
        start: 52912,
        len: 990,
        anchor: (9.045, 9.045),
    },
    Sprite {
        start: 53902,
        len: 416,
        anchor: (11.675, 11.675),
    },
    Sprite {
        start: 54318,
        len: 492,
        anchor: (11.675, 11.675),
    },
    Sprite {
        start: 54810,
        len: 394,
        anchor: (11.675, 11.675),
    },
    Sprite {
        start: 55204,
        len: 544,
        anchor: (11.675, 11.675),
    },
    Sprite {
        start: 55748,
        len: 426,
        anchor: (11.675, 11.675),
    },
    Sprite {
        start: 56174,
        len: 576,
        anchor: (11.675, 11.675),
    },
    Sprite {
        start: 56750,
        len: 406,
        anchor: (11.675, 11.675),
    },
    Sprite {
        start: 57156,
        len: 494,
        anchor: (11.675, 11.675),
    },
    Sprite {
        start: 57650,
        len: 882,
        anchor: (17.465, 17.465),
    },
    Sprite {
        start: 58532,
        len: 1172,
        anchor: (17.465, 17.465),
    },
    Sprite {
        start: 59704,
        len: 828,
        anchor: (17.465, 17.465),
    },
    Sprite {
        start: 60532,
        len: 1170,
        anchor: (17.465, 17.465),
    },
    Sprite {
        start: 61702,
        len: 854,
        anchor: (17.465, 17.465),
    },
    Sprite {
        start: 62556,
        len: 1198,
        anchor: (17.465, 17.465),
    },
    Sprite {
        start: 63754,
        len: 806,
        anchor: (17.465, 17.465),
    },
    Sprite {
        start: 64560,
        len: 1144,
        anchor: (17.465, 17.465),
    },
    Sprite {
        start: 65704,
        len: 414,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 66118,
        len: 398,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 66516,
        len: 408,
        anchor: (11.615, 23.22),
    },
    Sprite {
        start: 66924,
        len: 278,
        anchor: (11.875, 23.21),
    },
    Sprite {
        start: 67202,
        len: 94,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 67296,
        len: 94,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 67390,
        len: 92,
        anchor: (11.985, 23.22),
    },
    Sprite {
        start: 67482,
        len: 274,
        anchor: (12.485, 23.21),
    },
    Sprite {
        start: 67756,
        len: 398,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 68154,
        len: 388,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 68542,
        len: 256,
        anchor: (11.615, 23.22),
    },
    Sprite {
        start: 68798,
        len: 94,
        anchor: (11.875, 23.21),
    },
    Sprite {
        start: 68892,
        len: 94,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 68986,
        len: 94,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 69080,
        len: 332,
        anchor: (12.085, 23.22),
    },
    Sprite {
        start: 69412,
        len: 394,
        anchor: (12.485, 23.21),
    },
    Sprite {
        start: 69806,
        len: 322,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 70128,
        len: 324,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 70452,
        len: 268,
        anchor: (11.615, 23.22),
    },
    Sprite {
        start: 70720,
        len: 290,
        anchor: (11.875, 23.21),
    },
    Sprite {
        start: 71010,
        len: 94,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 71104,
        len: 94,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 71198,
        len: 92,
        anchor: (11.985, 23.22),
    },
    Sprite {
        start: 71290,
        len: 224,
        anchor: (12.485, 23.21),
    },
    Sprite {
        start: 71514,
        len: 320,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 71834,
        len: 270,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 72104,
        len: 224,
        anchor: (11.615, 23.22),
    },
    Sprite {
        start: 72328,
        len: 94,
        anchor: (11.875, 23.21),
    },
    Sprite {
        start: 72422,
        len: 94,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 72516,
        len: 94,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 72610,
        len: 238,
        anchor: (12.22, 23.22),
    },
    Sprite {
        start: 72848,
        len: 306,
        anchor: (12.485, 23.21),
    },
    Sprite {
        start: 73154,
        len: 218,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 73372,
        len: 238,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 73610,
        len: 268,
        anchor: (11.615, 23.22),
    },
    Sprite {
        start: 73878,
        len: 232,
        anchor: (11.875, 23.21),
    },
    Sprite {
        start: 74110,
        len: 94,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 74204,
        len: 94,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 74298,
        len: 92,
        anchor: (11.985, 23.22),
    },
    Sprite {
        start: 74390,
        len: 214,
        anchor: (12.485, 23.21),
    },
    Sprite {
        start: 74604,
        len: 240,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 74844,
        len: 232,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 75076,
        len: 184,
        anchor: (11.615, 23.22),
    },
    Sprite {
        start: 75260,
        len: 94,
        anchor: (11.875, 23.21),
    },
    Sprite {
        start: 75354,
        len: 94,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 75448,
        len: 94,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 75542,
        len: 200,
        anchor: (12.16, 23.22),
    },
    Sprite {
        start: 75742,
        len: 222,
        anchor: (12.485, 23.21),
    },
    Sprite {
        start: 75964,
        len: 172,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 76136,
        len: 190,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 76326,
        len: 160,
        anchor: (11.615, 23.22),
    },
    Sprite {
        start: 76486,
        len: 94,
        anchor: (11.875, 23.21),
    },
    Sprite {
        start: 76580,
        len: 94,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 76674,
        len: 94,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 76768,
        len: 158,
        anchor: (11.985, 23.22),
    },
    Sprite {
        start: 76926,
        len: 186,
        anchor: (12.485, 23.21),
    },
    Sprite {
        start: 77112,
        len: 202,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 77314,
        len: 170,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 77484,
        len: 200,
        anchor: (11.615, 23.22),
    },
    Sprite {
        start: 77684,
        len: 94,
        anchor: (11.875, 23.21),
    },
    Sprite {
        start: 77778,
        len: 94,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 77872,
        len: 94,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 77966,
        len: 136,
        anchor: (11.985, 23.22),
    },
    Sprite {
        start: 78102,
        len: 188,
        anchor: (12.485, 23.21),
    },
    Sprite {
        start: 78290,
        len: 260,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 78550,
        len: 226,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 78776,
        len: 232,
        anchor: (11.615, 23.22),
    },
    Sprite {
        start: 79008,
        len: 94,
        anchor: (11.875, 23.21),
    },
    Sprite {
        start: 79102,
        len: 94,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 79196,
        len: 94,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 79290,
        len: 92,
        anchor: (11.985, 23.22),
    },
    Sprite {
        start: 79382,
        len: 272,
        anchor: (12.485, 23.21),
    },
    Sprite {
        start: 79654,
        len: 1224,
        anchor: (12.675, 26.38),
    },
    Sprite {
        start: 80878,
        len: 1186,
        anchor: (12.435, 26.38),
    },
    Sprite {
        start: 82064,
        len: 1018,
        anchor: (11.615, 26.38),
    },
    Sprite {
        start: 83082,
        len: 1100,
        anchor: (11.875, 26.38),
    },
    Sprite {
        start: 84182,
        len: 1138,
        anchor: (12.675, 26.38),
    },
    Sprite {
        start: 85320,
        len: 1170,
        anchor: (12.435, 26.38),
    },
    Sprite {
        start: 86490,
        len: 1120,
        anchor: (11.985, 26.38),
    },
    Sprite {
        start: 87610,
        len: 1080,
        anchor: (12.485, 26.38),
    },
    Sprite {
        start: 88690,
        len: 340,
        anchor: (12.675, 26.38),
    },
    Sprite {
        start: 89030,
        len: 310,
        anchor: (12.435, 26.38),
    },
    Sprite {
        start: 89340,
        len: 314,
        anchor: (11.615, 26.38),
    },
    Sprite {
        start: 89654,
        len: 362,
        anchor: (11.875, 26.38),
    },
    Sprite {
        start: 90016,
        len: 292,
        anchor: (12.675, 26.38),
    },
    Sprite {
        start: 90308,
        len: 308,
        anchor: (12.435, 26.38),
    },
    Sprite {
        start: 90616,
        len: 346,
        anchor: (11.985, 26.38),
    },
    Sprite {
        start: 90962,
        len: 328,
        anchor: (12.485, 26.38),
    },
    Sprite {
        start: 91290,
        len: 998,
        anchor: (12.675, 23.925),
    },
    Sprite {
        start: 92288,
        len: 1040,
        anchor: (12.435, 23.925),
    },
    Sprite {
        start: 93328,
        len: 1074,
        anchor: (11.615, 23.925),
    },
    Sprite {
        start: 94402,
        len: 1016,
        anchor: (11.875, 23.925),
    },
    Sprite {
        start: 95418,
        len: 870,
        anchor: (12.675, 23.925),
    },
    Sprite {
        start: 96288,
        len: 958,
        anchor: (12.435, 23.925),
    },
    Sprite {
        start: 97246,
        len: 954,
        anchor: (15.77, 23.925),
    },
    Sprite {
        start: 98200,
        len: 1060,
        anchor: (14.905, 23.925),
    },
    Sprite {
        start: 99260,
        len: 1284,
        anchor: (12.675, 27.07),
    },
    Sprite {
        start: 100544,
        len: 1240,
        anchor: (12.435, 27.105),
    },
    Sprite {
        start: 101784,
        len: 1238,
        anchor: (11.615, 27.225),
    },
    Sprite {
        start: 103022,
        len: 1250,
        anchor: (11.875, 27.195),
    },
    Sprite {
        start: 104272,
        len: 1312,
        anchor: (12.675, 27.07),
    },
    Sprite {
        start: 105584,
        len: 1314,
        anchor: (12.435, 27.105),
    },
    Sprite {
        start: 106898,
        len: 1216,
        anchor: (11.985, 27.225),
    },
    Sprite {
        start: 108114,
        len: 1244,
        anchor: (12.485, 27.195),
    },
    Sprite {
        start: 109358,
        len: 578,
        anchor: (12.675, 27.07),
    },
    Sprite {
        start: 109936,
        len: 544,
        anchor: (12.435, 27.105),
    },
    Sprite {
        start: 110480,
        len: 534,
        anchor: (11.615, 27.225),
    },
    Sprite {
        start: 111014,
        len: 530,
        anchor: (11.875, 27.195),
    },
    Sprite {
        start: 111544,
        len: 590,
        anchor: (12.675, 27.07),
    },
    Sprite {
        start: 112134,
        len: 528,
        anchor: (12.435, 27.105),
    },
    Sprite {
        start: 112662,
        len: 510,
        anchor: (11.985, 27.225),
    },
    Sprite {
        start: 113172,
        len: 468,
        anchor: (12.485, 27.195),
    },
    Sprite {
        start: 113640,
        len: 906,
        anchor: (12.675, 26.335),
    },
    Sprite {
        start: 114546,
        len: 804,
        anchor: (12.435, 25.475),
    },
    Sprite {
        start: 115350,
        len: 746,
        anchor: (11.615, 25.56),
    },
    Sprite {
        start: 116096,
        len: 778,
        anchor: (11.875, 25.435),
    },
    Sprite {
        start: 116874,
        len: 758,
        anchor: (12.675, 25.56),
    },
    Sprite {
        start: 117632,
        len: 864,
        anchor: (12.435, 26.375),
    },
    Sprite {
        start: 118496,
        len: 836,
        anchor: (11.985, 26.88),
    },
    Sprite {
        start: 119332,
        len: 926,
        anchor: (12.485, 26.87),
    },
    Sprite {
        start: 120258,
        len: 830,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 121088,
        len: 820,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 121908,
        len: 542,
        anchor: (11.615, 23.22),
    },
    Sprite {
        start: 122450,
        len: 426,
        anchor: (11.875, 23.21),
    },
    Sprite {
        start: 122876,
        len: 94,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 122970,
        len: 324,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 123294,
        len: 432,
        anchor: (11.985, 23.22),
    },
    Sprite {
        start: 123726,
        len: 606,
        anchor: (12.485, 23.21),
    },
    Sprite {
        start: 124332,
        len: 1192,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 125524,
        len: 1142,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 126666,
        len: 960,
        anchor: (11.615, 23.22),
    },
    Sprite {
        start: 127626,
        len: 902,
        anchor: (11.875, 23.21),
    },
    Sprite {
        start: 128528,
        len: 986,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 129514,
        len: 1000,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 130514,
        len: 930,
        anchor: (11.985, 23.22),
    },
    Sprite {
        start: 131444,
        len: 1042,
        anchor: (12.485, 23.21),
    },
    Sprite {
        start: 132486,
        len: 732,
        anchor: (14.605, 25.56),
    },
    Sprite {
        start: 133218,
        len: 772,
        anchor: (14.605, 25.56),
    },
    Sprite {
        start: 133990,
        len: 786,
        anchor: (14.605, 25.56),
    },
    Sprite {
        start: 134776,
        len: 778,
        anchor: (14.605, 25.56),
    },
    Sprite {
        start: 135554,
        len: 748,
        anchor: (14.605, 25.56),
    },
    Sprite {
        start: 136302,
        len: 750,
        anchor: (14.605, 25.56),
    },
    Sprite {
        start: 137052,
        len: 778,
        anchor: (14.605, 25.56),
    },
    Sprite {
        start: 137830,
        len: 810,
        anchor: (14.605, 25.56),
    },
    Sprite {
        start: 138640,
        len: 1434,
        anchor: (14.605, 25.56),
    },
    Sprite {
        start: 140074,
        len: 1464,
        anchor: (14.605, 25.56),
    },
    Sprite {
        start: 141538,
        len: 1436,
        anchor: (14.605, 25.56),
    },
    Sprite {
        start: 142974,
        len: 1458,
        anchor: (14.605, 25.56),
    },
    Sprite {
        start: 144432,
        len: 1464,
        anchor: (14.605, 25.56),
    },
    Sprite {
        start: 145896,
        len: 1466,
        anchor: (14.605, 25.56),
    },
    Sprite {
        start: 147362,
        len: 1454,
        anchor: (14.605, 25.56),
    },
    Sprite {
        start: 148816,
        len: 1462,
        anchor: (14.605, 25.56),
    },
    Sprite {
        start: 150278,
        len: 1150,
        anchor: (12.675, 23.595),
    },
    Sprite {
        start: 151428,
        len: 1044,
        anchor: (12.435, 23.61),
    },
    Sprite {
        start: 152472,
        len: 1032,
        anchor: (11.805, 23.65),
    },
    Sprite {
        start: 153504,
        len: 1138,
        anchor: (12.815, 23.64),
    },
    Sprite {
        start: 154642,
        len: 1154,
        anchor: (12.86, 23.595),
    },
    Sprite {
        start: 155796,
        len: 1140,
        anchor: (12.435, 23.61),
    },
    Sprite {
        start: 156936,
        len: 1098,
        anchor: (11.985, 23.65),
    },
    Sprite {
        start: 158034,
        len: 1110,
        anchor: (12.485, 23.64),
    },
    Sprite {
        start: 159144,
        len: 402,
        anchor: (12.675, 23.595),
    },
    Sprite {
        start: 159546,
        len: 344,
        anchor: (12.435, 23.61),
    },
    Sprite {
        start: 159890,
        len: 302,
        anchor: (11.805, 23.65),
    },
    Sprite {
        start: 160192,
        len: 190,
        anchor: (12.815, 23.64),
    },
    Sprite {
        start: 160382,
        len: 94,
        anchor: (12.86, 23.595),
    },
    Sprite {
        start: 160476,
        len: 148,
        anchor: (12.435, 23.61),
    },
    Sprite {
        start: 160624,
        len: 192,
        anchor: (11.985, 23.65),
    },
    Sprite {
        start: 160816,
        len: 324,
        anchor: (12.485, 23.64),
    },
    Sprite {
        start: 161140,
        len: 1270,
        anchor: (13.24, 24.29),
    },
    Sprite {
        start: 162410,
        len: 1248,
        anchor: (13.025, 24.215),
    },
    Sprite {
        start: 163658,
        len: 1186,
        anchor: (12.32, 24.18),
    },
    Sprite {
        start: 164844,
        len: 1426,
        anchor: (12.58, 24.165),
    },
    Sprite {
        start: 166270,
        len: 1600,
        anchor: (13.24, 24.29),
    },
    Sprite {
        start: 167870,
        len: 1598,
        anchor: (13.025, 24.215),
    },
    Sprite {
        start: 169468,
        len: 1448,
        anchor: (12.9, 24.18),
    },
    Sprite {
        start: 170916,
        len: 1218,
        anchor: (12.97, 24.165),
    },
    Sprite {
        start: 172134,
        len: 1626,
        anchor: (13.24, 24.29),
    },
    Sprite {
        start: 173760,
        len: 1612,
        anchor: (13.025, 24.215),
    },
    Sprite {
        start: 175372,
        len: 1588,
        anchor: (12.32, 24.18),
    },
    Sprite {
        start: 176960,
        len: 1604,
        anchor: (12.58, 24.165),
    },
    Sprite {
        start: 178564,
        len: 1426,
        anchor: (13.24, 24.29),
    },
    Sprite {
        start: 179990,
        len: 1516,
        anchor: (13.025, 24.215),
    },
    Sprite {
        start: 181506,
        len: 1604,
        anchor: (12.9, 24.18),
    },
    Sprite {
        start: 183110,
        len: 1502,
        anchor: (12.97, 24.165),
    },
    Sprite {
        start: 184612,
        len: 716,
        anchor: (12.675, 23.45),
    },
    Sprite {
        start: 185328,
        len: 774,
        anchor: (12.435, 23.465),
    },
    Sprite {
        start: 186102,
        len: 796,
        anchor: (11.615, 23.975),
    },
    Sprite {
        start: 186898,
        len: 850,
        anchor: (11.875, 25.23),
    },
    Sprite {
        start: 187748,
        len: 812,
        anchor: (12.675, 25.365),
    },
    Sprite {
        start: 188560,
        len: 756,
        anchor: (12.435, 24.4),
    },
    Sprite {
        start: 189316,
        len: 778,
        anchor: (11.985, 23.51),
    },
    Sprite {
        start: 190094,
        len: 662,
        anchor: (12.485, 23.495),
    },
    Sprite {
        start: 190756,
        len: 498,
        anchor: (12.675, 26.715),
    },
    Sprite {
        start: 191254,
        len: 516,
        anchor: (12.435, 26.915),
    },
    Sprite {
        start: 191770,
        len: 408,
        anchor: (11.615, 27.285),
    },
    Sprite {
        start: 192178,
        len: 434,
        anchor: (11.875, 27.19),
    },
    Sprite {
        start: 192612,
        len: 514,
        anchor: (12.675, 26.715),
    },
    Sprite {
        start: 193126,
        len: 524,
        anchor: (12.435, 26.915),
    },
    Sprite {
        start: 193650,
        len: 410,
        anchor: (11.985, 27.285),
    },
    Sprite {
        start: 194060,
        len: 438,
        anchor: (12.485, 27.19),
    },
    Sprite {
        start: 194498,
        len: 588,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 195086,
        len: 476,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 195562,
        len: 406,
        anchor: (11.615, 23.22),
    },
    Sprite {
        start: 195968,
        len: 138,
        anchor: (11.875, 23.21),
    },
    Sprite {
        start: 196106,
        len: 378,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 196484,
        len: 448,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 196932,
        len: 518,
        anchor: (11.985, 23.22),
    },
    Sprite {
        start: 197450,
        len: 642,
        anchor: (12.485, 23.21),
    },
    Sprite {
        start: 198092,
        len: 374,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 198466,
        len: 318,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 198784,
        len: 92,
        anchor: (11.615, 23.22),
    },
    Sprite {
        start: 198876,
        len: 94,
        anchor: (11.875, 23.21),
    },
    Sprite {
        start: 198970,
        len: 94,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 199064,
        len: 302,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 199366,
        len: 388,
        anchor: (11.985, 23.22),
    },
    Sprite {
        start: 199754,
        len: 390,
        anchor: (12.485, 23.21),
    },
    Sprite {
        start: 200144,
        len: 682,
        anchor: (15.46, 23.53),
    },
    Sprite {
        start: 200826,
        len: 742,
        anchor: (14.475, 23.635),
    },
    Sprite {
        start: 201568,
        len: 538,
        anchor: (11.615, 24.055),
    },
    Sprite {
        start: 202106,
        len: 548,
        anchor: (11.875, 23.92),
    },
    Sprite {
        start: 202654,
        len: 672,
        anchor: (15.46, 23.53),
    },
    Sprite {
        start: 203326,
        len: 738,
        anchor: (14.475, 23.635),
    },
    Sprite {
        start: 204064,
        len: 520,
        anchor: (11.985, 24.055),
    },
    Sprite {
        start: 204584,
        len: 556,
        anchor: (12.485, 23.92),
    },
    Sprite {
        start: 205140,
        len: 1378,
        anchor: (15.46, 23.53),
    },
    Sprite {
        start: 206518,
        len: 1390,
        anchor: (14.475, 23.635),
    },
    Sprite {
        start: 207908,
        len: 900,
        anchor: (11.615, 24.055),
    },
    Sprite {
        start: 208808,
        len: 1054,
        anchor: (11.875, 23.92),
    },
    Sprite {
        start: 209862,
        len: 1406,
        anchor: (15.46, 23.53),
    },
    Sprite {
        start: 211268,
        len: 1422,
        anchor: (14.475, 23.635),
    },
    Sprite {
        start: 212690,
        len: 954,
        anchor: (11.985, 24.055),
    },
    Sprite {
        start: 213644,
        len: 1066,
        anchor: (12.485, 23.92),
    },
    Sprite {
        start: 214710,
        len: 292,
        anchor: (12.675, 28.935),
    },
    Sprite {
        start: 215002,
        len: 272,
        anchor: (12.435, 29.33),
    },
    Sprite {
        start: 215274,
        len: 274,
        anchor: (11.615, 29.535),
    },
    Sprite {
        start: 215548,
        len: 252,
        anchor: (11.875, 29.425),
    },
    Sprite {
        start: 215800,
        len: 246,
        anchor: (12.675, 29.065),
    },
    Sprite {
        start: 216046,
        len: 272,
        anchor: (12.435, 28.665),
    },
    Sprite {
        start: 216318,
        len: 250,
        anchor: (11.985, 28.465),
    },
    Sprite {
        start: 216568,
        len: 256,
        anchor: (12.485, 28.575),
    },
    Sprite {
        start: 216824,
        len: 318,
        anchor: (12.675, 28.935),
    },
    Sprite {
        start: 217142,
        len: 374,
        anchor: (12.435, 29.33),
    },
    Sprite {
        start: 217516,
        len: 346,
        anchor: (11.615, 29.535),
    },
    Sprite {
        start: 217862,
        len: 278,
        anchor: (11.875, 29.425),
    },
    Sprite {
        start: 218140,
        len: 336,
        anchor: (12.675, 29.065),
    },
    Sprite {
        start: 218476,
        len: 292,
        anchor: (12.435, 28.665),
    },
    Sprite {
        start: 218768,
        len: 266,
        anchor: (11.985, 28.465),
    },
    Sprite {
        start: 219034,
        len: 294,
        anchor: (12.485, 28.575),
    },
    Sprite {
        start: 219328,
        len: 804,
        anchor: (12.675, 29.32),
    },
    Sprite {
        start: 220132,
        len: 638,
        anchor: (12.435, 29.26),
    },
    Sprite {
        start: 220770,
        len: 710,
        anchor: (11.615, 29.645),
    },
    Sprite {
        start: 221480,
        len: 630,
        anchor: (11.875, 29.55),
    },
    Sprite {
        start: 222110,
        len: 792,
        anchor: (12.675, 29.32),
    },
    Sprite {
        start: 222902,
        len: 680,
        anchor: (12.435, 29.26),
    },
    Sprite {
        start: 223582,
        len: 616,
        anchor: (11.985, 29.645),
    },
    Sprite {
        start: 224198,
        len: 646,
        anchor: (12.485, 29.55),
    },
    Sprite {
        start: 224844,
        len: 580,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 225424,
        len: 422,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 225846,
        len: 368,
        anchor: (11.615, 23.22),
    },
    Sprite {
        start: 226214,
        len: 94,
        anchor: (11.875, 23.21),
    },
    Sprite {
        start: 226308,
        len: 326,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 226634,
        len: 400,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 227034,
        len: 420,
        anchor: (11.985, 23.22),
    },
    Sprite {
        start: 227454,
        len: 532,
        anchor: (12.485, 23.21),
    },
    Sprite {
        start: 227986,
        len: 1096,
        anchor: (12.85, 23.165),
    },
    Sprite {
        start: 229082,
        len: 1030,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 230112,
        len: 716,
        anchor: (11.615, 23.22),
    },
    Sprite {
        start: 230828,
        len: 478,
        anchor: (11.875, 23.21),
    },
    Sprite {
        start: 231306,
        len: 288,
        anchor: (12.775, 23.165),
    },
    Sprite {
        start: 231594,
        len: 242,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 231836,
        len: 472,
        anchor: (12.99, 23.22),
    },
    Sprite {
        start: 232308,
        len: 882,
        anchor: (13.63, 23.21),
    },
    Sprite {
        start: 233190,
        len: 1082,
        anchor: (12.85, 23.165),
    },
    Sprite {
        start: 234272,
        len: 1052,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 235324,
        len: 718,
        anchor: (11.615, 23.22),
    },
    Sprite {
        start: 236042,
        len: 470,
        anchor: (11.875, 23.21),
    },
    Sprite {
        start: 236512,
        len: 288,
        anchor: (12.775, 23.165),
    },
    Sprite {
        start: 236800,
        len: 242,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 237042,
        len: 472,
        anchor: (12.99, 23.22),
    },
    Sprite {
        start: 237514,
        len: 854,
        anchor: (13.63, 23.21),
    },
    Sprite {
        start: 238368,
        len: 508,
        anchor: (12.85, 23.165),
    },
    Sprite {
        start: 238876,
        len: 580,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 239456,
        len: 388,
        anchor: (11.615, 23.22),
    },
    Sprite {
        start: 239844,
        len: 248,
        anchor: (11.875, 23.21),
    },
    Sprite {
        start: 240092,
        len: 94,
        anchor: (12.775, 23.165),
    },
    Sprite {
        start: 240186,
        len: 94,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 240280,
        len: 128,
        anchor: (12.99, 23.22),
    },
    Sprite {
        start: 240408,
        len: 444,
        anchor: (13.63, 23.21),
    },
    Sprite {
        start: 240852,
        len: 442,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 241294,
        len: 468,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 241762,
        len: 370,
        anchor: (11.615, 23.22),
    },
    Sprite {
        start: 242132,
        len: 280,
        anchor: (11.875, 23.21),
    },
    Sprite {
        start: 242412,
        len: 94,
        anchor: (12.675, 23.165),
    },
    Sprite {
        start: 242506,
        len: 94,
        anchor: (12.435, 23.18),
    },
    Sprite {
        start: 242600,
        len: 272,
        anchor: (12.7, 23.22),
    },
    Sprite {
        start: 242872,
        len: 412,
        anchor: (12.485, 23.21),
    },
    Sprite {
        start: 243284,
        len: 964,
        anchor: (12.92, 23.165),
    },
    Sprite {
        start: 244248,
        len: 930,
        anchor: (12.7, 23.18),
    },
    Sprite {
        start: 245178,
        len: 914,
        anchor: (11.92, 23.22),
    },
    Sprite {
        start: 246092,
        len: 930,
        anchor: (12.165, 23.21),
    },
    Sprite {
        start: 247022,
        len: 848,
        anchor: (12.92, 23.165),
    },
    Sprite {
        start: 247870,
        len: 884,
        anchor: (12.7, 23.18),
    },
    Sprite {
        start: 248754,
        len: 998,
        anchor: (12.265, 23.22),
    },
    Sprite {
        start: 249752,
        len: 1062,
        anchor: (12.485, 23.21),
    },
    Sprite {
        start: 250814,
        len: 706,
        anchor: (5.485, 7.68),
    },
    Sprite {
        start: 251520,
        len: 734,
        anchor: (5.485, 7.68),
    },
    Sprite {
        start: 252254,
        len: 744,
        anchor: (5.485, 7.68),
    },
    Sprite {
        start: 252998,
        len: 762,
        anchor: (5.485, 7.68),
    },
    Sprite {
        start: 253760,
        len: 790,
        anchor: (5.485, 7.68),
    },
    Sprite {
        start: 254550,
        len: 402,
        anchor: (3.665, 4.61),
    },
    Sprite {
        start: 254952,
        len: 416,
        anchor: (3.665, 4.61),
    },
    Sprite {
        start: 255368,
        len: 416,
        anchor: (3.665, 4.61),
    },
    Sprite {
        start: 255784,
        len: 430,
        anchor: (3.665, 4.61),
    },
    Sprite {
        start: 256214,
        len: 434,
        anchor: (3.665, 4.61),
    },
    Sprite {
        start: 256648,
        len: 172,
        anchor: (1.44, 1.44),
    },
    Sprite {
        start: 256820,
        len: 252,
        anchor: (2.16, 2.16),
    },
    Sprite {
        start: 257072,
        len: 214,
        anchor: (1.56, 1.895),
    },
    Sprite {
        start: 257286,
        len: 406,
        anchor: (1.955, 5.54),
    },
    Sprite {
        start: 257692,
        len: 296,
        anchor: (2.64, 2.415),
    },
    Sprite {
        start: 257988,
        len: 240,
        anchor: (1.8, 1.8),
    },
    Sprite {
        start: 258228,
        len: 230,
        anchor: (1.8, 1.8),
    },
    Sprite {
        start: 258458,
        len: 382,
        anchor: (1.8, 4.475),
    },
    Sprite {
        start: 258840,
        len: 342,
        anchor: (2.875, 2.955),
    },
    Sprite {
        start: 259182,
        len: 350,
        anchor: (2.95, 2.77),
    },
    Sprite {
        start: 259532,
        len: 364,
        anchor: (2.88, 4.025),
    },
    Sprite {
        start: 259896,
        len: 362,
        anchor: (2.88, 4.025),
    },
    Sprite {
        start: 260258,
        len: 298,
        anchor: (1.145, 4.41),
    },
    Sprite {
        start: 260556,
        len: 244,
        anchor: (1.7, 1.635),
    },
    Sprite {
        start: 260800,
        len: 252,
        anchor: (1.58, 1.665),
    },
    Sprite {
        start: 261052,
        len: 210,
        anchor: (1.29, 1.615),
    },
    Sprite {
        start: 261262,
        len: 196,
        anchor: (1.445, 1.69),
    },
    Sprite {
        start: 261458,
        len: 294,
        anchor: (2.415, 2.79),
    },
    Sprite {
        start: 261752,
        len: 320,
        anchor: (2.815, 4.325),
    },
    Sprite {
        start: 262072,
        len: 374,
        anchor: (3.21, 5.865),
    },
    Sprite {
        start: 262446,
        len: 418,
        anchor: (3.61, 7.405),
    },
    Sprite {
        start: 262864,
        len: 644,
        anchor: (4.015, 9.905),
    },
    Sprite {
        start: 263508,
        len: 1806,
        anchor: (17.48, 17.48),
    },
    Sprite {
        start: 265314,
        len: 1720,
        anchor: (17.48, 17.48),
    },
    Sprite {
        start: 267034,
        len: 1580,
        anchor: (17.48, 17.48),
    },
    Sprite {
        start: 268614,
        len: 1588,
        anchor: (17.48, 17.48),
    },
    Sprite {
        start: 270202,
        len: 1614,
        anchor: (17.48, 17.48),
    },
    Sprite {
        start: 271816,
        len: 1720,
        anchor: (17.48, 17.48),
    },
    Sprite {
        start: 273536,
        len: 1716,
        anchor: (17.48, 17.48),
    },
    Sprite {
        start: 275252,
        len: 3126,
        anchor: (24.145, 24.145),
    },
    Sprite {
        start: 278378,
        len: 3142,
        anchor: (24.145, 24.145),
    },
    Sprite {
        start: 281520,
        len: 2946,
        anchor: (24.145, 24.145),
    },
    Sprite {
        start: 284466,
        len: 2874,
        anchor: (24.145, 24.145),
    },
    Sprite {
        start: 287340,
        len: 3114,
        anchor: (24.145, 24.145),
    },
    Sprite {
        start: 290454,
        len: 3088,
        anchor: (24.145, 24.145),
    },
    Sprite {
        start: 293542,
        len: 3116,
        anchor: (24.145, 24.145),
    },
    Sprite {
        start: 296658,
        len: 632,
        anchor: (4.27, 4.27),
    },
    Sprite {
        start: 297290,
        len: 664,
        anchor: (4.27, 4.27),
    },
    Sprite {
        start: 297954,
        len: 678,
        anchor: (4.27, 4.27),
    },
    Sprite {
        start: 298632,
        len: 688,
        anchor: (4.27, 4.27),
    },
    Sprite {
        start: 299320,
        len: 658,
        anchor: (4.27, 4.27),
    },
    Sprite {
        start: 299978,
        len: 646,
        anchor: (4.27, 4.27),
    },
    Sprite {
        start: 300624,
        len: 676,
        anchor: (4.27, 4.27),
    },
    Sprite {
        start: 301300,
        len: 636,
        anchor: (4.27, 4.27),
    },
    Sprite {
        start: 301936,
        len: 282,
        anchor: (11.0, 3.5),
    },
];

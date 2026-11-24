//! File and virtual offsets coincide in the pinned executable ELF segment.

pub const LIB_SHA256: &str = "69f4be20fb14ccc43297e2eb9e256e8fe1061746ffa71ec74cf1083d6f813218";

pub const FUNCTIONS: &[(&str, u32, u32)] = &[
    ("IGRP_DrawImage1_8Bit", 0xc4228, 7072),
    ("IGRP_DrawImage1_16Bit", 0xc2be0, 5704),
    ("RC4EnCode", 0xace4c, 1100),
    ("IGRP_FillRect", 0xc188c, 1916),
    ("B64DeCode", 0xacbd4, 544),
    ("Glyph_DrawSelf", 0xa4a4c, 1420),
    ("IGRP_DrawImage2_8Bit", 0xc7b40, 9440),
    ("IVO_Alloc", 0xe98e8, 672),
    ("IFS_SameData", 0xae1cc, 448),
];

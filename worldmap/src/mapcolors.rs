//! Vanilla map colours (`MapColor` id -> RGB), generated from Pumpkin's
//! `assets/map_colors.json` (tag 0.2.2+26.3-26.60). Do not edit by hand: the server reports
//! the id per block (`map-color` in the plugin API) and this table turns it into a colour.
//! Regenerate when a new Minecraft version adds colours (ids 62 and 63 are unused today).
pub const VANILLA: [(u8, u8, u8); 62] = [
    (0, 0, 0), // 0 none
    (127, 178, 56), // 1 grass
    (247, 233, 163), // 2 sand
    (199, 199, 199), // 3 wool
    (255, 0, 0), // 4 fire
    (160, 160, 255), // 5 ice
    (167, 167, 167), // 6 metal
    (0, 124, 0), // 7 plant
    (255, 255, 255), // 8 snow
    (164, 168, 184), // 9 clay
    (151, 109, 77), // 10 dirt
    (112, 112, 112), // 11 stone
    (64, 64, 255), // 12 water
    (143, 119, 72), // 13 wood
    (255, 252, 245), // 14 quartz
    (216, 127, 51), // 15 color_orange
    (178, 76, 216), // 16 color_magenta
    (102, 153, 216), // 17 color_light_blue
    (229, 229, 51), // 18 color_yellow
    (127, 204, 25), // 19 color_light_green
    (242, 127, 165), // 20 color_pink
    (76, 76, 76), // 21 color_gray
    (153, 153, 153), // 22 color_light_gray
    (76, 127, 153), // 23 color_cyan
    (127, 63, 178), // 24 color_purple
    (51, 76, 178), // 25 color_blue
    (102, 76, 51), // 26 color_brown
    (102, 127, 51), // 27 color_green
    (153, 51, 51), // 28 color_red
    (25, 25, 25), // 29 color_black
    (250, 238, 77), // 30 gold
    (92, 219, 213), // 31 diamond
    (74, 128, 255), // 32 lapis
    (0, 217, 58), // 33 emerald
    (129, 86, 49), // 34 podzol
    (112, 2, 0), // 35 nether
    (209, 177, 161), // 36 terracotta_white
    (159, 82, 36), // 37 terracotta_orange
    (149, 87, 108), // 38 terracotta_magenta
    (112, 108, 138), // 39 terracotta_light_blue
    (186, 133, 36), // 40 terracotta_yellow
    (103, 117, 53), // 41 terracotta_light_green
    (160, 77, 78), // 42 terracotta_pink
    (57, 41, 35), // 43 terracotta_gray
    (135, 107, 98), // 44 terracotta_light_gray
    (87, 92, 92), // 45 terracotta_cyan
    (122, 73, 88), // 46 terracotta_purple
    (76, 62, 92), // 47 terracotta_blue
    (76, 50, 35), // 48 terracotta_brown
    (76, 82, 42), // 49 terracotta_green
    (142, 60, 46), // 50 terracotta_red
    (37, 22, 16), // 51 terracotta_black
    (189, 48, 49), // 52 crimson_nylium
    (148, 63, 97), // 53 crimson_stem
    (92, 25, 29), // 54 crimson_hyphae
    (22, 126, 134), // 55 warped_nylium
    (58, 142, 140), // 56 warped_stem
    (86, 44, 62), // 57 warped_hyphae
    (20, 180, 133), // 58 warped_wart_block
    (100, 100, 100), // 59 deepslate
    (216, 175, 147), // 60 raw_iron
    (127, 167, 150), // 61 glow_lichen
];

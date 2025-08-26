// src/geohash.rs

use geohash::encode;
use geo::Coordinate;
use std::error::Error;

pub fn get_current_geohash() -> Result<String, Box<dyn Error>> {
    // ここではダミーの位置情報を使います
    // 将来的に、OSから位置情報を取得するロジックに置き換えます
    let lat = 35.6895; // 東京駅の緯度
    let lon = 139.6917; // 東京駅の経度
    let coord = Coordinate { x: lon, y: lat };

    // ジオハッシュをエンコード（精度を指定）
    let geohash_str = encode(coord, 6usize);

    Ok(geohash_str)
}

//! Static sea-level inundation and coastal morphology masks.
//! Not a wave, tide, sediment transport, or ocean hydrodynamics solver.
use crate::terrain::HeightField;
use std::collections::VecDeque;

pub struct SeaFields {
    pub height: HeightField,
    pub surface: HeightField,
    pub water: HeightField,
    pub wet: HeightField,
    pub shore: HeightField,
    pub beach: HeightField,
}

/// Shore distance is counted from actual adjoining wet/dry cells, not merely
/// from elevation proximity. A perfectly flat all-land or all-water map
/// produces no artificial shoreline.
pub fn coast(
    source:&HeightField, sea_level:f32, shore_width:u32, erosion:f32,
    cancelled:impl Fn()->bool,
)->Result<SeaFields,String> {
    let n=source.size;
    if n<2 || source.values.len()!=n.checked_mul(n)
        .ok_or("Sea dimensions overflow")?
        || source.values.iter().any(|v|!v.is_finite()) {
        return Err("Sea requires a complete finite heightfield of at least 2x2".into());
    }
    if !sea_level.is_finite() || !(-2.0..=4.0).contains(&sea_level)
        || !(1..=32).contains(&shore_width)
        || !erosion.is_finite() || !(0.0..=1.0).contains(&erosion) {
        return Err("Sea level must be -2..4, shore width 1..32 cells, erosion 0..1".into());
    }
    let length=n*n;
    let initial_wet:Vec<bool>=source.values.iter().map(|&v|v<sea_level).collect();
    let mut distance=vec![usize::MAX;length];
    let mut queue=VecDeque::new();

    for y in 0..n {
        if y%32==0 && cancelled(){return Err("Build cancelled".into());}
        for x in 0..n {
            let i=y*n+x;
            let mut edge=false;
            if x>0 && initial_wet[i]!=initial_wet[i-1]{edge=true;}
            if x+1<n && initial_wet[i]!=initial_wet[i+1]{edge=true;}
            if y>0 && initial_wet[i]!=initial_wet[i-n]{edge=true;}
            if y+1<n && initial_wet[i]!=initial_wet[i+n]{edge=true;}
            if edge {
                distance[i]=0;
                queue.push_back(i);
            }
        }
    }

    while let Some(i)=queue.pop_front() {
        if i%4096==0 && cancelled(){return Err("Build cancelled".into());}
        let d=distance[i];
        if d>=shore_width as usize {continue;}
        let x=i%n;
        let y=i/n;
        let neighbors=[
            if x>0 {Some(i-1)} else {None},
            if x+1<n {Some(i+1)} else {None},
            if y>0 {Some(i-n)} else {None},
            if y+1<n {Some(i+n)} else {None},
        ];
        for next in neighbors.into_iter().flatten() {
            if distance[next]>d+1 {
                distance[next]=d+1;
                queue.push_back(next);
            }
        }
    }

    let mut height=source.clone();
    let mut shore=HeightField::flat(n,0.0);
    for y in 0..n {
        if y%32==0 && cancelled(){return Err("Build cancelled".into());}
        for x in 0..n {
            let i=y*n+x;
            let d=distance[i];
            if d>shore_width as usize {continue;}
            let t=(1.0-d as f32/shore_width as f32).clamp(0.0,1.0);
            let weight=t*t*(3.0-2.0*t);
            shore.values[i]=weight;
            if erosion>0.0 {
                let mut sum=source.values[i];
                let mut count=1.0;
                for (xx,yy) in [(x.saturating_sub(1),y),
                    ((x+1).min(n-1),y),(x,y.saturating_sub(1)),
                    (x,(y+1).min(n-1))] {
                    if xx==x&&yy==y{continue;}
                    sum+=source.get(xx,yy);
                    count+=1.0;
                }
                let average=sum/count;
                height.values[i]=source.values[i]+(average-source.values[i])*weight*erosion;
            }
        }
    }

    let mut water=HeightField::flat(n,0.0);
    let mut wet=HeightField::flat(n,0.0);
    let mut beach=HeightField::flat(n,0.0);
    let mut surface=height.clone();
    let step=(n-1) as f32;
    for y in 0..n {
        if y%32==0 && cancelled(){return Err("Build cancelled".into());}
        for x in 0..n {
            let i=y*n+x;
            let bottom=height.values[i];
            let depth=(sea_level-bottom).max(0.0);
            water.values[i]=depth;
            wet.values[i]=if depth>0.0{1.0}else{0.0};
            surface.values[i]=bottom.max(sea_level);
            if depth==0.0 {
                let dx=(height.get((x+1).min(n-1),y)
                    -height.get(x.saturating_sub(1),y))*step*0.5;
                let dy=(height.get(x,(y+1).min(n-1))
                    -height.get(x,y.saturating_sub(1)))*step*0.5;
                let slope=(dx*dx+dy*dy).sqrt();
                beach.values[i]=shore.values[i]/(1.0+3.0*slope);
            }
        }
    }
    if cancelled(){return Err("Build cancelled".into());}
    Ok(SeaFields{height,surface,water,wet,shore,beach})
}

#[cfg(test)]
mod tests {
    use super::*;

    fn island(n:usize)->HeightField {
        HeightField {
            size:n,
            values:(0..n).flat_map(|y|(0..n).map(move|x|{
                let u=x as f32/(n-1) as f32-0.5;
                let v=y as f32/(n-1) as f32-0.5;
                (0.8-1.4*(u*u+v*v).sqrt()).max(0.0)
            })).collect()
        }
    }

    #[test]
    fn coastline_follows_wet_dry_transition_and_has_flat_sea_surface() {
        let land=island(64);
        let sea=coast(&land,0.25,5,0.3,||false).unwrap();
        let same=coast(&land,0.25,5,0.3,||false).unwrap();
        assert_eq!(sea.height,same.height);
        assert_eq!(sea.water,same.water);
        assert!(sea.water.get(0,0)>0.0);
        assert_eq!(sea.water.get(32,32),0.0);
        assert_eq!(sea.surface.get(0,0),0.25);
        assert!(sea.wet.values.iter().any(|&v|v==1.0));
        assert!(sea.wet.values.iter().any(|&v|v==0.0));
        assert!(sea.shore.values.iter().any(|&v|v>0.0));
        assert!(sea.beach.values.iter().any(|&v|v>0.0));
        for map in [&sea.wet,&sea.shore,&sea.beach] {
            assert!(map.values.iter().all(|v|v.is_finite()&&(0.0..=1.0).contains(v)));
        }
        assert!(sea.water.values.iter().all(|v|v.is_finite()&&*v>=0.0));
    }

    #[test]
    fn no_coast_on_uniformly_dry_or_submerged_map_and_zero_erosion_is_identity() {
        let dry=HeightField::flat(32,0.8);
        let wet=HeightField::flat(32,0.1);
        for land in [dry,wet] {
            let sea=coast(&land,0.5,6,0.8,||false).unwrap();
            assert_eq!(sea.height,land);
            assert!(sea.shore.values.iter().all(|&v|v==0.0));
            assert!(sea.beach.values.iter().all(|&v|v==0.0));
        }
        let coastless=coast(&island(32),0.3,8,0.0,||false).unwrap();
        assert_eq!(coastless.height,island(32));
    }

    #[test]
    fn invalid_levels_or_widths_and_cancellation_are_rejected() {
        let terrain=island(64);
        assert!(coast(&terrain,f32::NAN,4,0.2,||false).is_err());
        assert!(coast(&terrain,0.3,0,0.2,||false).is_err());
        assert!(coast(&terrain,0.3,33,0.2,||false).is_err());
        assert!(coast(&terrain,0.3,4,2.0,||false).is_err());
        assert!(coast(&terrain,0.3,4,0.2,||true).is_err());
    }
}

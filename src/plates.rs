//! Seeded Voronoi plate regions with independent convergence/uplift masks.
//! A deterministic kinematic terrain primitive, NOT geophysical evolution.
use crate::terrain::{fractal, HeightField};

pub struct PlateFields {
    pub height: HeightField,
    pub boundaries: HeightField,
    pub convergence: HeightField,
    pub divergence: HeightField,
    pub plate_ids: HeightField,
    pub continental: HeightField,
}
#[derive(Clone, Copy)]
struct Plate {
    x: f32, y: f32, vx: f32, vy: f32,
    continental: bool,
}
fn random(seed:u32,index:u32,channel:u32)->f32 {
    let mut n = seed ^ index.wrapping_mul(0x9e3779b9)
        ^ channel.wrapping_mul(0x85ebca6b);
    n ^= n>>16;
    n = n.wrapping_mul(0x7feb352d);
    n ^= n>>15;
    n = n.wrapping_mul(0x846ca68b);
    n ^= n>>16;
    n as f32 / u32::MAX as f32
}
fn seed_plates(seed:u32,count:usize,continent_fraction:f32)->Vec<Plate> {
    let columns=(count as f32).sqrt().ceil() as usize;
    let rows=count.div_ceil(columns);
    (0..count).map(|i|{
        let u=(i%columns) as f32;
        let v=(i/columns) as f32;
        let angle=random(seed,i as u32,2)*std::f32::consts::TAU;
        let speed=0.3+0.7*random(seed,i as u32,3);
        Plate {
            x:(u+0.18+0.64*random(seed,i as u32,0))/columns as f32,
            y:(v+0.18+0.64*random(seed,i as u32,1))/rows as f32,
            vx:speed*angle.cos(), vy:speed*angle.sin(),
            continental:random(seed,i as u32,4)<continent_fraction,
        }
    }).collect()
}

pub fn plates(
    size:usize, seed:u32, count:u32, continent_fraction:f32,
    uplift:f32, warp:f32, cancelled:impl Fn()->bool,
)->Result<PlateFields,String> {
    if size<2 || !(2..=64).contains(&count)
        || !continent_fraction.is_finite()
        || !(0.0..=1.0).contains(&continent_fraction)
        || !uplift.is_finite() || !(0.0..=2.0).contains(&uplift)
        || !warp.is_finite() || !(0.0..=1.0).contains(&warp) {
        return Err("Plates needs size >=2, count 2..64, continent 0..1, uplift 0..2, warp 0..1".into());
    }
    let models=seed_plates(seed,count as usize,continent_fraction);
    let mut out=PlateFields{
        height:HeightField::flat(size,0.0),
        boundaries:HeightField::flat(size,0.0),
        convergence:HeightField::flat(size,0.0),
        divergence:HeightField::flat(size,0.0),
        plate_ids:HeightField::flat(size,0.0),
        continental:HeightField::flat(size,0.0),
    };
    let n=(size-1) as f32;
    let boundary_width=(0.075/(count as f32/10.0).sqrt()).clamp(0.018,0.12);
    for y in 0..size {
        if y%16==0 && cancelled() {return Err("Build cancelled".into());}
        for x in 0..size {
            let u=x as f32/n;
            let v=y as f32/n;
            let xx=u+(fractal(u,v,seed.wrapping_add(113),3.0)-0.5)*warp*0.25;
            let yy=v+(fractal(u,v,seed.wrapping_add(229),3.0)-0.5)*warp*0.25;
            let mut first=(f32::INFINITY,0usize);
            let mut second=(f32::INFINITY,0usize);
            for (i,p) in models.iter().enumerate() {
                let dx=xx-p.x;
                let dy=yy-p.y;
                let d=dx*dx+dy*dy;
                if d<first.0 {
                    second=first; first=(d,i);
                } else if d<second.0 {
                    second=(d,i);
                }
            }
            let a=models[first.1];
            let b=models[second.1];
            let bx=b.x-a.x;
            let by=b.y-a.y;
            let length=(bx*bx+by*by).sqrt().max(0.001);
            // Nearest and runner-up distances provide a smooth approximation
            // of distance to the warped Voronoi plate boundary.
            let edge_distance=(second.0.sqrt()-first.0.sqrt()).max(0.0)*0.5;
            let edge=(-0.5*(edge_distance/boundary_width).powi(2)).exp();
            let approach=((a.vx-b.vx)*bx+(a.vy-b.vy)*by)/length;
            let colliding=edge*(approach/2.0).clamp(0.0,1.0);
            let spreading=edge*(-approach/2.0).clamp(0.0,1.0);
            let broad=fractal(u,v,seed.wrapping_add(311),2.6);
            let terrain_base=if a.continental {
                0.35+0.20*broad
            } else {
                0.05+0.12*broad
            };
            let relief=(fractal(u,v,seed.wrapping_add(417),10.0)-0.5)
                *0.12*uplift;
            let height=(terrain_base+uplift*(0.55*colliding-0.23*spreading)
                +relief).clamp(0.0,3.0);
            let i=y*size+x;
            out.height.values[i]=height;
            out.boundaries.values[i]=edge;
            out.convergence.values[i]=colliding;
            out.divergence.values[i]=spreading;
            out.plate_ids.values[i]=first.1 as f32/(models.len()-1) as f32;
            out.continental.values[i]=if a.continental{1.0}else{0.0};
        }
    }
    if cancelled(){return Err("Build cancelled".into());}
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn produces_deterministic_seeded_regions_and_both_boundary_regimes() {
        let a=plates(64,42,12,0.55,0.8,0.5,||false).unwrap();
        let b=plates(64,42,12,0.55,0.8,0.5,||false).unwrap();
        let c=plates(64,43,12,0.55,0.8,0.5,||false).unwrap();
        assert_eq!(a.height,b.height);
        assert_eq!(a.convergence,b.convergence);
        assert_ne!(a.height,c.height);
        assert!(a.boundaries.values.iter().any(|&v|v>0.9));
        assert!(a.convergence.values.iter().any(|&v|v>0.01));
        assert!(a.divergence.values.iter().any(|&v|v>0.01));
        assert!(a.plate_ids.values.iter().any(|&v|v>0.0 && v<1.0));
        assert!(a.continental.values.iter().any(|&v|v==1.0));
        assert!(a.continental.values.iter().any(|&v|v==0.0));
        for map in [&a.boundaries,&a.convergence,&a.divergence,
                    &a.plate_ids,&a.continental] {
            assert!(map.values.iter().all(|v|v.is_finite()&&(0.0..=1.0).contains(v)));
        }
        assert!(a.height.values.iter().all(|v|v.is_finite()&&*v>=0.0));
    }

    #[test]
    fn uplift_strength_changes_landform_but_not_plate_labels() {
        let flat=plates(32,21,9,0.5,0.0,0.35,||false).unwrap();
        let uplift=plates(32,21,9,0.5,1.2,0.35,||false).unwrap();
        assert_eq!(flat.plate_ids,uplift.plate_ids);
        assert_eq!(flat.convergence,uplift.convergence);
        assert_ne!(flat.height,uplift.height);
    }

    #[test]
    fn rejects_invalid_parameters_and_cancels() {
        assert!(plates(64,1,1,0.5,0.4,0.1,||false).is_err());
        assert!(plates(64,1,65,0.5,0.4,0.1,||false).is_err());
        assert!(plates(64,1,12,f32::NAN,0.4,0.1,||false).is_err());
        assert!(plates(64,1,12,0.5,3.0,0.1,||false).is_err());
        assert!(plates(64,1,12,0.5,0.4,0.1,||true).is_err());
    }
}

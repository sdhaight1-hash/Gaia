//! Deterministic terrain-derived ambient visibility and directional sunlight.
//! Scalar masks for materials/DCC output; not physically based path tracing.
use crate::terrain::HeightField;

const DIRS:[(isize,isize);8]=[
    (-1,0),(1,0),(0,-1),(0,1),
    (-1,-1),(1,-1),(-1,1),(1,1)
];

fn validate(input:&HeightField)->Result<(),String> {
    if input.size<2 || input.values.len()!=input.size.checked_mul(input.size)
        .ok_or("Lighting dimensions overflow")?
        || input.values.iter().any(|v|!v.is_finite()) {
        Err("Lighting requires a finite square heightfield >=2".into())
    } else { Ok(()) }
}

/// AO is a bounded angular horizon estimate in eight directions.
/// A flat plane returns 1; deep hollows are darker than exposed ridges.
pub fn ambient_occlusion(
    input:&HeightField,radius:u32,strength:f32,cancelled:impl Fn()->bool,
)->Result<HeightField,String> {
    validate(input)?;
    if !(1..=64).contains(&radius)
        || !strength.is_finite() || !(0.0..=1.0).contains(&strength) {
        return Err("AO radius must be 1–64 and strength 0–1".into());
    }
    let n=input.size;
    let mut out=HeightField::flat(n,1.0);
    let cell=1.0/(n-1) as f32;
    for y in 0..n {
        if y%16==0 && cancelled(){return Err("Build cancelled".into());}
        for x in 0..n {
            let base=input.get(x,y);
            let mut total=0.0;
            for (dx,dy) in DIRS {
                let mut maximum=0.0_f32;
                let length=(dx*dx+dy*dy) as f32;
                let step=cell*length.sqrt();
                for k in 1..=radius {
                    let xx=x as isize+dx*k as isize;
                    let yy=y as isize+dy*k as isize;
                    if xx<0 || yy<0 || xx>=n as isize || yy>=n as isize {break;}
                    let rise=input.get(xx as usize, yy as usize)-base;
                    maximum=maximum.max((rise/(step*k as f32)).max(0.0));
                }
                total+=maximum.atan()/std::f32::consts::FRAC_PI_2;
            }
            out.values[y*n+x]=(1.0-strength*total/8.0).clamp(0.0,1.0);
        }
    }
    if cancelled(){return Err("Build cancelled".into());}
    Ok(out)
}

/// Sun azimuth: 0° north (towards -image Y), 90° east. The output is
/// diffuse lighting times terrain self-shadow visibility in [0,1].
pub fn sunlight(
    input:&HeightField,azimuth:f32,elevation:f32,distance:u32,
    cancelled:impl Fn()->bool,
)->Result<HeightField,String> {
    validate(input)?;
    if !azimuth.is_finite() || !(0.0..=360.0).contains(&azimuth)
        || !elevation.is_finite() || !(1.0..=89.0).contains(&elevation)
        || !(1..=128).contains(&distance) {
        return Err("Sunlight requires azimuth 0–360°, elevation 1–89°, distance 1–128".into());
    }
    let n=input.size;
    let cell=1.0/(n-1) as f32;
    let bearing=azimuth.to_radians();
    let altitude=elevation.to_radians();
    let (sin_az,cos_az)=bearing.sin_cos();
    let ray_x=sin_az;
    let ray_y=-cos_az;
    let sun_x=ray_x*altitude.cos();
    let sun_y=ray_y*altitude.cos();
    let sun_z=altitude.sin();
    let mut out=HeightField::flat(n,0.0);
    for y in 0..n {
        if y%16==0 && cancelled(){return Err("Build cancelled".into());}
        for x in 0..n {
            let base=input.get(x,y);
            let dx=(input.get((x+1).min(n-1),y)
                -input.get(x.saturating_sub(1),y))*(n-1) as f32*0.5;
            let dy=(input.get(x,(y+1).min(n-1))
                -input.get(x,y.saturating_sub(1)))*(n-1) as f32*0.5;
            let inv=(dx*dx+dy*dy+1.0).sqrt().recip();
            let diffuse=((-dx*sun_x-dy*sun_y+sun_z)*inv).clamp(0.0,1.0);
            let mut occluded=false;
            for step in 1..=distance {
                let sx=x as f32+ray_x*step as f32;
                let sy=y as f32+ray_y*step as f32;
                let ix=sx.round() as isize;
                let iy=sy.round() as isize;
                if ix<0 || iy<0 || ix>=n as isize || iy>=n as isize {break;}
                let ray_height=base+step as f32*cell*altitude.tan();
                if input.get(ix as usize,iy as usize)>ray_height+1e-5 {
                    occluded=true;
                    break;
                }
            }
            out.values[y*n+x]=if occluded{diffuse*0.08}else{diffuse};
        }
    }
    if cancelled(){return Err("Build cancelled".into());}
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flat_surface_has_full_ao_and_directional_light_is_bounded() {
        let flat=HeightField::flat(32,0.3);
        let ao=ambient_occlusion(&flat,12,0.8,||false).unwrap();
        assert!(ao.values.iter().all(|&v|v==1.0));
        let sun=sunlight(&flat,45.0,45.0,24,||false).unwrap();
        assert!(sun.values.iter().all(|&v|(v-std::f32::consts::FRAC_1_SQRT_2).abs()<1e-6));
        assert!(sun.values.iter().all(|v|v.is_finite()&&(0.0..=1.0).contains(v)));
    }

    #[test]
    fn enclosed_pit_has_darker_ao_than_exposed_plateau() {
        let n=33;
        let mut field=HeightField::flat(n,0.8);
        field.values[(n/2)*n+n/2]=0.1;
        let shade=ambient_occlusion(&field,8,1.0,||false).unwrap();
        assert!(shade.get(n/2,n/2)<shade.get(0,0));
        assert_eq!(shade,ambient_occlusion(&field,8,1.0,||false).unwrap());
        let neutral=ambient_occlusion(&field,8,0.0,||false).unwrap();
        assert!(neutral.values.iter().all(|&v|v==1.0));
    }

    #[test]
    fn ridge_casts_shadow_opposite_sun() {
        let n=33;
        let mut field=HeightField::flat(n,0.0);
        for x in 0..n {field.values[12*n+x]=1.0;}
        let mask=sunlight(&field,0.0,20.0,32,||false).unwrap();
        assert!(mask.get(16,20)<mask.get(16,3)*0.2);
        assert_eq!(mask,sunlight(&field,0.0,20.0,32,||false).unwrap());
    }

    #[test]
    fn errors_on_invalid_inputs_and_cancellation() {
        let flat=HeightField::flat(32,0.3);
        assert!(ambient_occlusion(&flat,0,0.5,||false).is_err());
        assert!(ambient_occlusion(&flat,8,f32::NAN,||false).is_err());
        assert!(sunlight(&flat,0.0,0.0,16,||false).is_err());
        assert!(sunlight(&flat,0.0,45.0,0,||false).is_err());
        assert!(sunlight(&flat,0.0,45.0,16,||true).is_err());
        let mut invalid=flat; invalid.values[3]=f32::INFINITY;
        assert!(ambient_occlusion(&invalid,8,0.5,||false).is_err());
    }
}

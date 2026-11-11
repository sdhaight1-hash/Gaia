//! Independent deterministic snow accumulation, melting and settling.
//! Not Gaea's proprietary particle/energy-balance snow solver.
use crate::terrain::{fractal, HeightField};

pub struct SnowFields {
    pub height: HeightField,
    pub snow_depth: HeightField,
    pub coverage: HeightField,
    pub snowfall: f64,
    pub melted: f64,
    pub remaining: f64,
}

pub fn accumulate_snow(
    source: &HeightField, seed: u32, snowline: f32, intensity: f32,
    melt: f32, adherence: f32, settle: u32,
    cancelled: impl Fn() -> bool,
) -> Result<SnowFields, String> {
    let n = source.size;
    if n < 2 || source.values.len() != n.checked_mul(n).ok_or("Snow grid overflow")?
        || source.values.iter().any(|v| !v.is_finite()) {
        return Err("Snow requires a finite square heightfield of at least 2x2".into());
    }
    if !snowline.is_finite() || !(-2.0..=4.0).contains(&snowline)
        || !intensity.is_finite() || !(0.0..=0.5).contains(&intensity)
        || !melt.is_finite() || !(0.0..=1.0).contains(&melt)
        || !adherence.is_finite() || !(0.0..=1.0).contains(&adherence)
        || settle > 24 {
        return Err("Snow parameters outside snowline -2..4, intensity 0..0.5, melt/adhesion 0..1, settle 0..24".into());
    }
    let mut snow_depth = HeightField::flat(n, 0.0);
    let (mut snowfall, mut melted) = (0.0_f64, 0.0_f64);
    let inv_step = (n-1) as f32;
    for y in 0..n {
        if y%32 == 0 && cancelled() { return Err("Build cancelled".into()); }
        for x in 0..n {
            let h = source.get(x,y);
            let above = ((h-snowline)/0.10).clamp(0.0,1.0);
            let cold = above*above*(3.0-2.0*above);
            let dx = (source.get((x+1).min(n-1),y)
                -source.get(x.saturating_sub(1),y))*inv_step*0.5;
            let dy = (source.get(x,(y+1).min(n-1))
                -source.get(x,y.saturating_sub(1)))*inv_step*0.5;
            let slope = (dx*dx+dy*dy).sqrt();
            let retention = adherence+(1.0-adherence)/(1.0+2.0*slope);
            let variation = 0.75+0.5*fractal(x as f32/inv_step,
                y as f32/inv_step,seed,8.0);
            let deposited = intensity*cold*retention*variation;
            snowfall += f64::from(deposited);
            let thaw = deposited*melt;
            melted += f64::from(thaw);
            snow_depth.values[y*n+x] = deposited-thaw;
        }
    }
    let mut delta = vec![0.0_f32;n*n];
    for _ in 0..settle {
        if cancelled() { return Err("Build cancelled".into()); }
        delta.fill(0.0);
        for y in 0..n {
            if y%16 == 0 && cancelled() { return Err("Build cancelled".into()); }
            for x in 0..n {
                let i=y*n+x;
                let depth=snow_depth.values[i];
                if depth<=0.0 {continue;}
                let head=source.values[i]+depth;
                let mut receiver=None;
                let mut steepest=0.012_f32;
                for (dx,dy) in [(-1_isize,0_isize),(0,-1),(1,0),(0,1)] {
                    let xx=x as isize+dx;
                    let yy=y as isize+dy;
                    if xx<0 || yy<0 || xx>=n as isize || yy>=n as isize {continue;}
                    let next=yy as usize*n+xx as usize;
                    let drop=head-source.values[next]-snow_depth.values[next];
                    if drop>steepest { steepest=drop; receiver=Some(next); }
                }
                if let Some(next)=receiver {
                    let fraction=(0.22+0.38*(1.0-adherence))
                        *(steepest/(steepest+0.08));
                    let amount=depth*fraction.min(0.6);
                    delta[i]-=amount;
                    delta[next]+=amount;
                }
            }
        }
        for (value,change) in snow_depth.values.iter_mut().zip(&delta) {
            *value=(*value+change).max(0.0);
        }
    }
    let mut height=source.clone();
    let mut coverage=HeightField::flat(n,0.0);
    for (i,&snow) in snow_depth.values.iter().enumerate() {
        height.values[i]+=snow;
        coverage.values[i]=(snow/intensity.max(1e-6)).clamp(0.0,1.0);
    }
    let remaining=snow_depth.mass();
    Ok(SnowFields{height,snow_depth,coverage,snowfall,melted,remaining})
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mountain(n:usize)->HeightField {
        HeightField{size:n,values:(0..n).flat_map(|y|(0..n).map(move|x|{
            let u=x as f32/(n-1) as f32-0.5;
            let v=y as f32/(n-1) as f32-0.5;
            (1.0-1.8*(u*u+v*v).sqrt()).max(0.0)
        })).collect()}
    }

    #[test]
    fn high_ground_gets_snow_and_lowland_stays_bare() {
        let source=mountain(64);
        let snow=accumulate_snow(&source,17,0.4,0.12,0.0,0.7,0,||false).unwrap();
        assert!(snow.snow_depth.get(32,32)>0.04);
        assert_eq!(snow.snow_depth.get(0,0),0.0);
        assert_eq!(snow.height.get(0,0),source.get(0,0));
        assert!(snow.coverage.values.iter().all(|v|(0.0..=1.0).contains(v)));
    }

    #[test]
    fn settling_conserves_post_melt_mass_and_is_deterministic() {
        let src=mountain(64);
        let a=accumulate_snow(&src,19,0.15,0.2,0.16,0.1,0,||false).unwrap();
        let b=accumulate_snow(&src,19,0.15,0.2,0.16,0.1,10,||false).unwrap();
        assert_ne!(a.snow_depth,b.snow_depth);
        assert!((b.snowfall-b.melted-b.remaining).abs()<0.001);
        assert!((a.remaining-b.remaining).abs()<0.001);
        assert_eq!(b.snow_depth,accumulate_snow(&src,19,0.15,0.2,0.16,
            0.1,10,||false).unwrap().snow_depth);
        assert!(b.height.values.iter().all(|v|v.is_finite()));
    }

    #[test]
    fn zero_snow_and_full_melt_leave_terrain_unchanged() {
        let src=mountain(32);
        assert_eq!(accumulate_snow(&src,1,0.1,0.2,1.0,0.5,8,||false)
            .unwrap().height,src);
        assert_eq!(accumulate_snow(&src,1,0.1,0.0,0.0,0.5,8,||false)
            .unwrap().height,src);
    }

    #[test]
    fn rejects_bad_data_or_controls_and_cancels() {
        let src=mountain(64);
        assert!(accumulate_snow(&src,1,f32::NAN,0.1,0.1,0.5,0,||false).is_err());
        assert!(accumulate_snow(&src,1,0.1,0.6,0.1,0.5,0,||false).is_err());
        assert!(accumulate_snow(&src,1,0.1,0.1,0.1,0.5,25,||false).is_err());
        assert!(accumulate_snow(&src,1,0.1,0.1,0.1,0.5,1,||true).is_err());
        let mut bad=src; bad.values[0]=f32::INFINITY;
        assert!(accumulate_snow(&bad,1,0.1,0.1,0.1,0.5,1,||false).is_err());
    }
}

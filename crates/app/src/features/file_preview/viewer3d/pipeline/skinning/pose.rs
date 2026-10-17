use gmpublished_domain::math::Vec3;
use std::collections::BTreeMap;
use vformats::mdl::ModelSkinVertex;

type Matrix = [[f32; 4]; 4];

#[derive(Debug)]
pub(in super::super) struct Centroid {
    bones: Vec<(u32, Vec3, f32)>,
    fallback: Vec3,
    count: usize,
}

impl Centroid {
    pub fn new(vertices: &[ModelSkinVertex]) -> Self {
        let mut bones = BTreeMap::<u32, (Vec3, f32)>::new();
        let mut fallback = Vec3::ZERO;
        for vertex in vertices {
            let position = Vec3::from(vertex.vertex.position);
            if vertex.bone_count == 0 {
                fallback += position;
                continue;
            }
            for slot in 0..vertex.bone_count as usize {
                let weight = vertex.weights[slot];
                if weight <= f32::EPSILON {
                    continue;
                }
                let entry = bones.entry(vertex.bones[slot]).or_insert((Vec3::ZERO, 0.0));
                entry.0 += position * weight;
                entry.1 += weight;
            }
        }
        Self {
            bones: bones
                .into_iter()
                .map(|(bone, (position, weight))| (bone, position, weight))
                .collect(),
            fallback,
            count: vertices.len(),
        }
    }

    pub fn sum(&self, palette: &[Matrix]) -> Vec3 {
        let mut result = self.fallback;
        for &(bone, position, weight) in &self.bones {
            let matrix = &palette[bone as usize];
            result += transform(matrix, position, weight);
        }
        result
    }

    pub fn count(&self) -> usize {
        self.count
    }
}

fn transform(matrix: &Matrix, value: Vec3, w: f32) -> Vec3 {
    Vec3::from(std::array::from_fn(|row| {
        matrix[0][row] * value[0]
            + matrix[1][row] * value[1]
            + matrix[2][row] * value[2]
            + matrix[3][row] * w
    }))
}

pub(super) fn position(vertex: &ModelSkinVertex, palette: &[Matrix]) -> Vec3 {
    let position = Vec3::from(vertex.vertex.position);
    if vertex.bone_count == 0 {
        return position;
    }
    let mut result = Vec3::ZERO;
    for slot in 0..vertex.bone_count as usize {
        let weight = vertex.weights[slot];
        if weight > f32::EPSILON {
            result += transform(&palette[vertex.bones[slot] as usize], position, 1.0) * weight;
        }
    }
    result
}

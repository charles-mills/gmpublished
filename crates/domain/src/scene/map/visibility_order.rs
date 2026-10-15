use super::super::{MapBsp, MapLeafLocator};
use crate::math::Vec3;
use std::ops::Range;

#[derive(Clone, Copy, Debug, PartialEq)]
struct Face {
    plane: u16,
    back: bool,
    node: bool,
    displacement: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct FaceOrdering {
    root: i32,
    node_faces: Vec<Range<u32>>,
    leaf_ranges: Vec<Range<usize>>,
    leaf_faces: Vec<u16>,
    faces: Vec<Face>,
}

impl FaceOrdering {
    pub(super) fn from_bsp(bsp: &MapBsp) -> Self {
        Self {
            root:bsp.models.first().map_or(0,|model|model.head_node),
            node_faces:bsp.nodes.iter().map(|node|u32::from(node.first_face)..u32::from(node.first_face)+u32::from(node.face_count)).collect(),
            leaf_ranges:bsp.leaves.iter().map(|leaf|usize::from(leaf.first_leaf_face)..usize::from(leaf.first_leaf_face)+usize::from(leaf.leaf_face_count)).collect(),
            leaf_faces:bsp.leaf_faces.clone(),
            faces:bsp.faces.iter().map(|face|Face {plane:face.plane,back:face.side!=0,node:face.on_node!=0,displacement:face.displacement>=0}).collect(),
        }
    }

    pub(super) fn ordered_faces(&self, locator:&MapLeafLocator, eye:Vec3, visible:impl Fn(u32)->bool)->Vec<u32> {
        enum Visit {Child(i32),Faces(usize,bool)}
        let mut stack=vec![Visit::Child(self.root)];
        let mut visited=vec![false;locator.nodes.len()];
        let mut marked=vec![false;self.faces.len()];
        let mut emitted=vec![false;self.faces.len()];
        let mut faces=Vec::new();
        while let Some(visit)=stack.pop() {
            match visit {
                Visit::Child(child) if child<0=>{
                    let leaf=(-i64::from(child)-1) as usize;
                    let Some(range)=self.leaf_ranges.get(leaf) else {continue;};
                    let Some(indices)=self.leaf_faces.get(range.clone()) else {continue;};
                    for &index in indices {
                        let index=usize::from(index);
                        let Some(face)=self.faces.get(index) else {continue;};
                        if visible(index as u32)&&face.node {marked[index]=true;}
                    }
                    for displacement in [true,false] {
                        for &index in indices {
                            let index=usize::from(index);
                            let Some(face)=self.faces.get(index) else {continue;};
                            if face.node||face.displacement!=displacement||emitted[index]||!visible(index as u32) {continue;}
                            let Some(plane)=locator.planes.get(usize::from(face.plane)) else {continue;};
                            if !displacement&&eye.dot(plane.normal)-plane.dist< -0.01 {continue;}
                            emitted[index]=true;
                            faces.push(index as u32);
                        }
                    }
                }
                Visit::Child(child)=>{
                    let Ok(index)=usize::try_from(child) else {continue;};
                    let Some(node)=locator.nodes.get(index) else {continue;};
                    if visited[index] {continue;}
                    visited[index]=true;
                    let Some(plane)=usize::try_from(node.plane_index).ok().and_then(|index|locator.planes.get(index)) else {continue;};
                    let back=eye.dot(plane.normal)-plane.dist<0.0;
                    let side=usize::from(back);
                    stack.push(Visit::Child(node.children[1-side]));
                    stack.push(Visit::Faces(index,back));
                    stack.push(Visit::Child(node.children[side]));
                }
                Visit::Faces(node,back)=>{
                    let Some(range)=self.node_faces.get(node) else {continue;};
                    for index in range.clone() {
                        let Some(face)=self.faces.get(index as usize) else {continue;};
                        if marked[index as usize]&&!emitted[index as usize]&&face.back==back&&visible(index) {
                            emitted[index as usize]=true;
                            faces.push(index);
                        }
                    }
                }
            }
        }
        faces
    }
}

#[cfg(test)]
mod tests;

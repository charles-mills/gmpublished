use super::ModelVertex;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, serde::Serialize)]
pub struct GeometryCounts {
    pub vertices: usize,
    pub indices: usize,
    pub vertex_bytes: usize,
    pub index_bytes: usize,
    pub mesh_ranges: usize,
}

impl GeometryCounts {
    pub fn payload_bytes(self) -> Result<usize, GeometryError> {
        self.vertex_bytes
            .checked_add(self.index_bytes)
            .ok_or(GeometryError::Overflow {
                component: GeometryComponent::TotalBytes,
            })
    }

    pub(in crate::media) fn checked(
        vertices: usize,
        indices: usize,
        mesh_ranges: usize,
    ) -> Result<Self, GeometryError> {
        let bytes = |count: usize, stride, component| {
            count
                .checked_mul(stride)
                .filter(|bytes| *bytes <= isize::MAX as usize)
                .ok_or(GeometryError::Overflow { component })
        };
        Ok(Self {
            vertices,
            indices,
            mesh_ranges,
            vertex_bytes: bytes(
                vertices,
                size_of::<ModelVertex>(),
                GeometryComponent::VertexBytes,
            )?,
            index_bytes: bytes(indices, size_of::<u32>(), GeometryComponent::IndexBytes)?,
        })
    }

    pub(in crate::media) fn checked_add(self, rhs: Self) -> Result<Self, GeometryError> {
        let add = |a: usize, b, component| {
            a.checked_add(b)
                .ok_or(GeometryError::Overflow { component })
        };
        let counts = Self {
            vertices: add(self.vertices, rhs.vertices, GeometryComponent::Vertices)?,
            indices: add(self.indices, rhs.indices, GeometryComponent::Indices)?,
            vertex_bytes: add(
                self.vertex_bytes,
                rhs.vertex_bytes,
                GeometryComponent::VertexBytes,
            )?,
            index_bytes: add(
                self.index_bytes,
                rhs.index_bytes,
                GeometryComponent::IndexBytes,
            )?,
            mesh_ranges: add(
                self.mesh_ranges,
                rhs.mesh_ranges,
                GeometryComponent::MeshRanges,
            )?,
        };
        counts.payload_bytes()?;
        Ok(counts)
    }

    pub(in crate::media) fn append_buffer(self, rhs: Self) -> Result<Self, GeometryError> {
        let next = self.checked_add(rhs)?;
        for (count, component) in [
            (next.vertices, GeometryComponent::Vertices),
            (next.indices, GeometryComponent::Indices),
            (next.mesh_ranges, GeometryComponent::MeshRanges),
        ] {
            u32::try_from(count).map_err(|_| GeometryError::Overflow { component })?;
        }
        Self::checked(next.vertices, next.indices, next.mesh_ranges)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize)]
pub enum GeometryComponent {
    Vertices,
    Indices,
    VertexBytes,
    IndexBytes,
    MeshRanges,
    TotalBytes,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
pub enum GeometryError {
    EmptyGeometry,
    NonTriangular {
        mesh: usize,
        indices: usize,
    },
    IndexOutOfBounds {
        mesh: usize,
        offset: usize,
        index: u32,
        vertices: usize,
    },
    NonFiniteVertex {
        mesh: usize,
        vertex: usize,
    },
    NonFinitePlacement,
    NonFiniteTransform {
        mesh: usize,
        vertex: usize,
    },
    MissingMaterial {
        mesh: usize,
        slot: usize,
    },
    Overflow {
        component: GeometryComponent,
    },
    Allocation {
        material_index: usize,
        entity: bool,
        component: GeometryComponent,
        bytes: usize,
        detail: String,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
pub struct GeometryDiagnostic {
    pub placement_index: usize,
    pub reason: GeometryError,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, serde::Serialize)]
pub struct PropGeometryReport {
    pub planned: GeometryCounts,
    pub admitted: GeometryCounts,
    pub rejected: Vec<GeometryDiagnostic>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, serde::Serialize)]
pub struct PropGeometrySummary {
    pub world: PropGeometryReport,
    pub skybox: PropGeometryReport,
    pub doors: PropGeometryReport,
}

impl PropGeometrySummary {
    #[cfg(any(test, feature = "map-capture"))]
    pub fn admitted_totals(&self) -> Result<GeometryCounts, GeometryError> {
        self.world
            .admitted
            .checked_add(self.skybox.admitted)?
            .checked_add(self.doors.admitted)
    }
}

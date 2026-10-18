use std::collections::BTreeMap;

use gmpublished_backend::model_project::SkinMaterial;

use super::State;

impl State {
    pub fn preview_skin_materials(&self) -> Option<(&BTreeMap<String, SkinMaterial>, &[String])> {
        let inspection = self.inspection.as_ref()?;
        let imported = inspection.skins.len().max(1);
        let skin = self.skins.get(self.workspace.skin.checked_sub(imported)?)?;
        let materials = inspection
            .skins
            .first()
            .filter(|slots| !slots.is_empty())
            .unwrap_or(&inspection.materials);
        Some((&skin.replacements, materials.as_slice()))
    }
}

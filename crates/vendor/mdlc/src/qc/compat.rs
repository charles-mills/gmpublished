use super::{Parser, QcError, Token};

impl Parser<'_> {
    pub(super) fn dispatch_compat(&mut self, token: &Token) -> Result<bool, QcError> {
        match token.text.to_ascii_lowercase().as_str() {
            "$internalname" => self.desc.model.internal_name = Some(self.tok(false)?.text),
            "$addsearchdir" => {
                let directory = self.tok(false)?.text.replace('\\', "/");
                self.lex.search_paths.push(self.qdir.join(&directory));
                self.desc.model.source_search_paths.push(directory);
            }
            "$qcassert" => self.qc_assert(token)?,
            "$appendsource" => self.append_source(token)?,
            "$fakevta" => self.fake_vta(token)?,
            "$redefineattachment" => self.redefine_attachment(token)?,
            "$centerbonesonverts" => self.desc.model.center_bones_on_vertices = true,
            "$collapsebones" => self.desc.model.collapse_bones = true,
            "$collapsebonesaggressive" => {
                self.desc.model.collapse_bones = true;
                self.desc.model.collapse_bones_aggressive = true;
            }
            "$alwayscollapse" => {
                self.desc.model.collapse_bones = true;
                let bone = self.tok(false)?.text;
                self.desc.model.always_collapse.push(bone);
            }
            "$insertbone" => {
                let child = self.tok(false)?.text;
                self.tok(false)?;
                let parent = self.tok(false)?.text;
                self.desc.model.bone_hierarchy.push([child, parent]);
            }
            "$screenalign" | "$worldalign" => {
                let bone = self.tok(false)?.text;
                let flags = if token.text.eq_ignore_ascii_case("$worldalign") {
                    0x20
                } else if !self.avail() {
                    0x08
                } else {
                    let mode = self.tok(false)?;
                    match mode.text.to_ascii_lowercase().as_str() {
                        "sphere" => 0x08,
                        "cylinder" => 0x10,
                        _ => {
                            return Err(QcError::new(
                                mode.file,
                                mode.line,
                                "Screen alignment must be sphere or cylinder",
                            ));
                        }
                    }
                };
                self.desc.model.screen_alignments.push((bone, flags));
            }
            "$maxverts" => {
                let threshold = self.i()?.clamp(1024, 65536) as usize;
                let target = if self.avail() {
                    self.i()?.clamp(1024, 65536) as usize
                } else {
                    threshold.min(32768)
                };
                self.desc.model.max_vertices = Some([threshold, target]);
            }
            "$lockbonelengths" => self.desc.model.lock_bone_lengths = true,
            "$bonesaveframe" => {
                let bone = self.tok(false)?.text;
                let mut setting = crate::model::BoneSaveFrame {
                    bone,
                    position: false,
                    rotation: false,
                };
                while self.avail() {
                    let option = self.tok(false)?;
                    match option.text.to_ascii_lowercase().as_str() {
                        "position" => setting.position = true,
                        "rotation" => setting.rotation = true,
                        _ => {
                            return Err(QcError::new(
                                option.file,
                                option.line,
                                "Expected position or rotation in $bonesaveframe",
                            ));
                        }
                    }
                }
                self.desc.model.bone_save_frames.push(setting);
            }
            // Authored LOD weights are preserved by default rather than copied from LOD 0.
            "$skinnedlods" => {}
            "$hgroup" => {
                let group = self.i()?;
                let bone = self.tok(false)?.text;
                self.desc.model.hitgroups.push((bone, group));
            }
            "$clampworldspace" => {
                let min = self.v3()?;
                let max = self.v3()?;
                if !(0..3).all(|axis| {
                    min[axis].is_finite() && max[axis].is_finite() && min[axis] <= max[axis]
                }) {
                    return Err(QcError::new(
                        &token.file,
                        token.line,
                        "World bounds must be finite and ordered",
                    ));
                }
                self.desc.model.clamp_worldspace = Some([min, max]);
            }
            "$bboxonlyverts" => self.desc.model.bbox_only_vertices = true,
            "$overridematerial" => {
                let material = self.tok(false)?.text;
                self.desc.model.material_renames.extend(
                    self.texture_order
                        .iter()
                        .map(|name| [name.clone(), material.clone()]),
                );
            }
            "$boneflexdriver" => {
                let bone = self.tok(false)?.text;
                let component = self.tok(false)?;
                let component = match component.text.to_ascii_lowercase().as_str() {
                    "tx" => 0,
                    "ty" => 1,
                    "tz" => 2,
                    _ => {
                        return Err(QcError::new(
                            component.file,
                            component.line,
                            "Bone flex drivers require tx, ty or tz",
                        ));
                    }
                };
                let controller = self.tok(false)?.text;
                let min = self.f()?;
                let max = self.f()?;
                if !min.is_finite() || !max.is_finite() || min == max {
                    return Err(QcError::new(
                        &token.file,
                        token.line,
                        "Bone flex driver endpoints must be finite and distinct",
                    ));
                }
                self.desc
                    .model
                    .bone_flex_drivers
                    .push(crate::model::BoneFlexDriver {
                        bone,
                        component,
                        controller,
                        min,
                        max,
                    });
            }
            "$obsolete" => self.gflags |= 0x200,
            "$subd" => self.gflags |= 1 << 19,
            "$zbrush" => self.desc.model.zbrush = true,
            "$preservetriangleorder" => self.desc.model.preserve_triangle_order = true,
            "$gamma" => {
                let value = self.f()?;
                if !value.is_finite() || value <= 0.0 {
                    return Err(QcError::new(
                        &token.file,
                        token.line,
                        "Gamma must be finite and greater than zero",
                    ));
                }
            }
            "$externaltextures" => {
                crate::warning!("$externaltextures is obsolete and has no effect")
            }
            "$cliptotextures" => {}
            "$phyname" => self.desc.physics.name = Some(self.tok(false)?.text),
            _ => return Ok(false),
        }
        Ok(true)
    }

    fn qc_assert(&mut self, token: &Token) -> Result<(), QcError> {
        let kind = self.tok(false)?;
        let bone = self.tok(false)?.text;
        let actual = match kind.text.to_ascii_lowercase().as_str() {
            "boneexists" => {
                let source = self.tok(false)?.text;
                let path = crate::compile::resolve_smd_path(&self.qdir, &self.resolve_src(&source))
                    .map_err(|error| QcError::new(&token.file, token.line, error.to_string()))?;
                let source = crate::compile::read_source(
                    &path,
                    "$qcassert",
                    &crate::compile::SrcOpts::default(),
                )
                .map_err(|error| QcError::new(&token.file, token.line, error.to_string()))?;
                self.desc.model.source_dependencies.push(path);
                source
                    .nodes
                    .iter()
                    .any(|node| node.name.eq_ignore_ascii_case(&bone))
            }
            "importboneexists" => self
                .import_bones
                .iter()
                .any(|defined| defined.name.eq_ignore_ascii_case(&bone)),
            _ => {
                return Err(QcError::new(
                    kind.file,
                    kind.line,
                    "Unknown QC assertion type",
                ));
            }
        };
        let expected = self.tok(false)?;
        let expected_value = match expected.text.to_ascii_lowercase().as_str() {
            "true" => true,
            "false" => false,
            _ => {
                return Err(QcError::new(
                    expected.file,
                    expected.line,
                    "QC assertion expects true or false",
                ));
            }
        };
        if actual != expected_value {
            return Err(QcError::new(
                &token.file,
                token.line,
                format!(
                    "QC assertion failed: {} {bone} {}",
                    kind.text, expected.text
                ),
            ));
        }
        Ok(())
    }

    fn redefine_attachment(&mut self, token: &Token) -> Result<(), QcError> {
        let name = self.tok(false)?;
        let index = self
            .desc
            .attachments
            .iter()
            .position(|attachment| attachment.name.eq_ignore_ascii_case(&name.text))
            .ok_or_else(|| {
                QcError::new(
                    &token.file,
                    token.line,
                    format!("Cannot redefine missing attachment {:?}", name.text),
                )
            })?;
        self.lex.unget(name);
        self.cmd_attachment()?;
        let replacement = self.desc.attachments.pop().expect("attachment was parsed");
        self.desc.attachments[index] = replacement;
        Ok(())
    }

    fn append_source(&mut self, token: &Token) -> Result<(), QcError> {
        if self.desc.model.first_source.is_none() {
            return Err(QcError::new(
                &token.file,
                token.line,
                "$appendsource requires a preceding model source",
            ));
        }
        let source = self.tok(false)?;
        let source = self.resolve_src(&source.text);
        let mut offset = None;
        if self.avail() {
            let option = self.tok(false)?;
            if option.text.starts_with("offset") {
                offset = Some(parse_source_offset(&option.text).ok_or_else(|| {
                    QcError::new(
                        option.file,
                        option.line,
                        "Expected offset pos[ x y z ] angle[ pitch yaw roll ] scale[ value ]",
                    )
                })?);
            } else {
                self.lex.unget(option);
            }
        }
        self.referenced_files.push(source.clone());
        self.mesh_sources.insert(source.clone());
        self.desc
            .model
            .append_sources
            .push(crate::source_support::AppendSource {
                source,
                input_scale: self.default_scale,
                offset,
            });
        Ok(())
    }

    fn fake_vta(&mut self, token: &Token) -> Result<(), QcError> {
        let name = self.tok(false)?.text;
        let name = self.resolve_src(&name);
        let open = self.tok(true)?;
        if open.text != "{" {
            return Err(QcError::new(
                open.file,
                open.line,
                "$fakevta requires an appendvta block",
            ));
        }
        let mut frames = Vec::new();
        loop {
            let command = self.tok(true)?;
            if command.text == "}" {
                break;
            }
            if !command.text.eq_ignore_ascii_case("appendvta") {
                return Err(QcError::new(
                    command.file,
                    command.line,
                    "Expected appendvta in $fakevta",
                ));
            }
            let source = self.tok(false)?.text;
            let source = self.resolve_src(&source);
            let frame = self.i()?;
            if !(0..100_000).contains(&frame) {
                return Err(QcError::new(
                    &token.file,
                    token.line,
                    "Fake VTA frame must be between 0 and 99999",
                ));
            }
            self.desc
                .model
                .source_dependencies
                .push(self.qdir.join(&source));
            frames.push(crate::source_support::VirtualVtaFrame { source, frame });
        }
        if self
            .desc
            .model
            .virtual_vtas
            .iter()
            .any(|vta| vta.name.eq_ignore_ascii_case(&name))
        {
            return Err(QcError::new(
                &token.file,
                token.line,
                "Duplicate fake VTA source",
            ));
        }
        self.desc
            .model
            .virtual_vtas
            .push(crate::source_support::VirtualVta { name, frames });
        Ok(())
    }
}

fn parse_source_offset(text: &str) -> Option<crate::source_support::SourceOffset> {
    let spaced = text.replace('[', " [ ").replace(']', " ] ");
    let words: Vec<_> = spaced.split_whitespace().collect();
    if words.len() != 17
        || words[0] != "offset"
        || words[1] != "pos"
        || words[2] != "["
        || words[6] != "]"
        || words[7] != "angle"
        || words[8] != "["
        || words[12] != "]"
        || words[13] != "scale"
        || words[14] != "["
        || words[16] != "]"
    {
        return None;
    }
    let number = |index: usize| {
        words[index]
            .parse::<f32>()
            .ok()
            .filter(|value| value.is_finite())
    };
    let scale = number(15)?;
    if scale <= 0.0 {
        return None;
    }
    Some(crate::source_support::SourceOffset {
        position: [number(3)?, number(4)?, number(5)?],
        rotation: [number(9)?, number(10)?, number(11)?],
        scale,
    })
}

#[cfg(test)]
#[path = "compat_tests.rs"]
mod tests;

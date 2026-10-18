These minimal ASCII FBX 7.4 fixtures isolate transform behavior without external assets. Axes are explicitly Z-up, +X right, and -Y front.

- `inherited-scale.fbx`: a nonuniformly scaled parent, a child using FBX RrSs inheritance, and an offset rigid triangle; its empty idle stack must match the native evaluated reference hierarchy.
- `skin-bind.fbx`: the mesh node is currently translated 20 units, but its skin cluster binds the triangle at 5 units with an identity bone bind.
- `animated-skin-bind.fbx`: the same skin bind with a bone translation from 0 to 3 units.
- `animated-scale.fbx`: one attached triangle and a scale channel from 1 to 2.
- `animated-shear.fbx`: a rotating child beneath nonuniform scale with RSrs inheritance, producing changing shear.

The import tests inspect intermediate geometry and poses. Backend workflow tests additionally compile MDL/VVD/VTX and check decoded geometry and animation deformation.

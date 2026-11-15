/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! Implementation of OpenGL ES 1.1 on top of OpenGL ES 2.0.
//!
//! OpenGL ES 2.0 is what WebGL is based on, so this is the implementation
//! that makes touchHLE usable in a web browser. It also works with native
//! OpenGL ES 2.0 (or later) drivers.
//!
//! OpenGL ES 2.0 removed the "fixed-function pipeline" of 1.1: there are no
//! matrix stacks, no `glVertexPointer` and friends, no texture environment,
//! no alpha test, no lighting and no fog. All drawing goes through shaders.
//! So this layer keeps track of all that state itself, and draws with a
//! shader program that reproduces the fixed-function behaviour.
//!
//! What is emulated: the matrix stacks, the vertex, colour and texture
//! coordinate arrays (and the current colour), two texture units with the
//! simple texture environment modes, and the alpha test.
//!
//! What is not (yet): lighting, materials, fog, user clip planes, the
//! `GL_COMBINE` texture environment, point parameters and sprites, matrix
//! palettes, flat shading and logic ops. Enabling or configuring these is
//! accepted, so that apps which merely touch them keep working, but it has no
//! effect on rendering and is logged once.
//!
//! Everything that still exists in OpenGL ES 2.0 is passed straight through.
//!
//! Useful resources:
//! - [OpenGL ES 2.0 reference pages](https://registry.khronos.org/OpenGL-Refpages/es2.0/)
//! - Section 3.7.12 "Texture Environments and Texture Functions" and section
//!   2.10 "Coordinate Transformations" of the OpenGL ES 1.1 specification.

use super::gles11_raw as gles11; // constants and types
use super::gles11_raw::types::*;
use super::gles20_raw as gl;
use super::util::{
    fixed_to_float, matrix_fixed_to_float, try_decode_pvrtc, PalettedTextureFormat,
};
use super::GLES;
use crate::window::{GLContext, GLVersion, Window};
use std::collections::{HashMap, HashSet};
use std::ffi::CStr;
use std::sync::Mutex;

/// `GL_BGRA_EXT`, from `EXT_texture_format_BGRA8888`. Apple's version of that
/// extension (which apps use) differs from the standard one, and WebGL has
/// neither, so this is always converted.
const BGRA: GLenum = 0x80E1;
/// `GL_POINT_SPRITE_OES`
const POINT_SPRITE: GLenum = 0x8861;

const TEXTURE_UNITS: usize = 2;

/// Capabilities that still exist in OpenGL ES 2.0.
const PASSED_THROUGH_CAPABILITIES: &[GLenum] = &[
    gles11::BLEND,
    gles11::CULL_FACE,
    gles11::DEPTH_TEST,
    gles11::DITHER,
    gles11::POLYGON_OFFSET_FILL,
    gles11::SAMPLE_ALPHA_TO_COVERAGE,
    gles11::SAMPLE_COVERAGE,
    gles11::SCISSOR_TEST,
    gles11::STENCIL_TEST,
];
/// Capabilities that are remembered but have no effect, see the module
/// documentation.
const IGNORED_CAPABILITIES: &[GLenum] = &[
    gles11::COLOR_LOGIC_OP,
    gles11::CLIP_PLANE0,
    gles11::CLIP_PLANE1,
    gles11::CLIP_PLANE2,
    gles11::CLIP_PLANE3,
    gles11::CLIP_PLANE4,
    gles11::CLIP_PLANE5,
    gles11::LIGHT0,
    gles11::LIGHT1,
    gles11::LIGHT2,
    gles11::LIGHT3,
    gles11::LIGHT4,
    gles11::LIGHT5,
    gles11::LIGHT6,
    gles11::LIGHT7,
    gles11::COLOR_MATERIAL,
    gles11::FOG,
    gles11::LIGHTING,
    gles11::LINE_SMOOTH,
    gles11::MULTISAMPLE,
    gles11::NORMALIZE,
    gles11::POINT_SMOOTH,
    gles11::RESCALE_NORMAL,
    gles11::SAMPLE_ALPHA_TO_ONE,
    POINT_SPRITE,
];

// Vertex attribute locations in the shader program.
const ATTRIB_POSITION: GLuint = 0;
const ATTRIB_COLOR: GLuint = 1;
const ATTRIB_TEX_COORD: [GLuint; TEXTURE_UNITS] = [2, 3];

const VERTEX_SHADER: &str = "\
attribute vec4 a_position;
attribute vec4 a_color;
attribute vec4 a_tex_coord0;
attribute vec4 a_tex_coord1;
uniform mat4 u_mvp;
uniform mat4 u_tex_matrix0;
uniform mat4 u_tex_matrix1;
uniform float u_point_size;
varying vec4 v_color;
varying vec4 v_tex_coord0;
varying vec4 v_tex_coord1;
void main() {
    gl_Position = u_mvp * a_position;
    v_color = clamp(a_color, 0.0, 1.0);
    v_tex_coord0 = u_tex_matrix0 * a_tex_coord0;
    v_tex_coord1 = u_tex_matrix1 * a_tex_coord1;
    gl_PointSize = u_point_size;
}
";

// The texture environment functions depend on which components the texture's
// format has: a texture without colour (GL_ALPHA) leaves the colour of the
// fragment alone, and one without alpha (GL_RGB, GL_LUMINANCE) leaves the
// alpha alone. u_tex_formatN is (has colour, has alpha).
//
// Texture environment modes: 0 = no texturing, 1 = MODULATE, 2 = REPLACE,
// 3 = DECAL, 4 = BLEND, 5 = ADD.
//
// Alpha test functions: 0 = always pass (or disabled), otherwise the GL
// constant minus GL_NEVER plus one.
//
// The shader is compiled in several variants, with TEXTURE0 and/or TEXTURE1
// defined for the texture units that are in use. A variant must not even
// declare the sampler of a unit it doesn't use: WebGL rejects a draw call if
// any sampler of the program refers to a texture that is attached to the
// framebuffer being drawn to, whether or not it would be sampled. With
// OpenGL ES 1.1 it is normal for such a texture to be left bound to a unit
// that has texturing disabled.
const FRAGMENT_SHADER: &str = "\
precision mediump float;
#ifdef TEXTURE0
uniform sampler2D u_texture0;
#endif
#ifdef TEXTURE1
uniform sampler2D u_texture1;
#endif
uniform int u_tex_env0;
uniform int u_tex_env1;
uniform vec4 u_tex_env_color0;
uniform vec4 u_tex_env_color1;
uniform vec2 u_tex_format0;
uniform vec2 u_tex_format1;
uniform int u_alpha_func;
uniform float u_alpha_ref;
varying vec4 v_color;
varying vec4 v_tex_coord0;
varying vec4 v_tex_coord1;

vec4 tex_env(int mode, vec4 prev, vec4 tex, vec2 format, vec4 env_color) {
    bool has_color = format.x > 0.5;
    bool has_alpha = format.y > 0.5;
    float alpha = has_alpha ? prev.a * tex.a : prev.a;
    if (mode == 1) {
        return vec4(has_color ? prev.rgb * tex.rgb : prev.rgb, alpha);
    } else if (mode == 2) {
        return vec4(has_color ? tex.rgb : prev.rgb, has_alpha ? tex.a : prev.a);
    } else if (mode == 3) {
        return vec4(has_alpha ? mix(prev.rgb, tex.rgb, tex.a) : tex.rgb, prev.a);
    } else if (mode == 4) {
        return vec4(has_color ? mix(prev.rgb, env_color.rgb, tex.rgb) : prev.rgb, alpha);
    } else {
        return vec4(has_color ? min(prev.rgb + tex.rgb, 1.0) : prev.rgb, alpha);
    }
}

void main() {
    vec4 color = v_color;
#ifdef TEXTURE0
    color = tex_env(u_tex_env0, color, texture2D(u_texture0, v_tex_coord0.xy),
                    u_tex_format0, u_tex_env_color0);
#endif
#ifdef TEXTURE1
    color = tex_env(u_tex_env1, color, texture2D(u_texture1, v_tex_coord1.xy),
                    u_tex_format1, u_tex_env_color1);
#endif
    if (u_alpha_func != 0) {
        float a = color.a;
        bool pass;
        if (u_alpha_func == 1) pass = false;
        else if (u_alpha_func == 2) pass = a < u_alpha_ref;
        else if (u_alpha_func == 3) pass = a == u_alpha_ref;
        else if (u_alpha_func == 4) pass = a <= u_alpha_ref;
        else if (u_alpha_func == 5) pass = a > u_alpha_ref;
        else if (u_alpha_func == 6) pass = a != u_alpha_ref;
        else pass = a >= u_alpha_ref;
        if (!pass) discard;
    }
    gl_FragColor = color;
}
";

/// Things that are known about textures but can't be asked of OpenGL ES 2.0.
/// This is global rather than per-context because contexts can share their
/// textures.
#[derive(Default)]
struct TextureInfo {
    /// Base internal format (`GL_RGBA` etc), by texture name.
    formats: HashMap<GLuint, GLenum>,
    /// Textures with the `GL_GENERATE_MIPMAP` parameter set.
    generate_mipmap: HashSet<GLuint>,
}
static TEXTURE_INFO: Mutex<Option<TextureInfo>> = Mutex::new(None);
fn with_texture_info<R>(f: impl FnOnce(&mut TextureInfo) -> R) -> R {
    f(TEXTURE_INFO.lock().unwrap().get_or_insert_with(Default::default))
}

/// Whether every [GLES1OnGLES2] instance is in fact using one and the same
/// underlying context.
///
/// touchHLE relies on having several independent contexts: at least one for
/// the app and one for its own use (compositing, presenting frames). A web
/// page's canvas only has a single WebGL context though, and Emscripten gives
/// out that same context every time one is "created". So there, the state
/// that this layer passes straight through to the real context has to be
/// swapped whenever a different instance is made current. See [RealState].
const SHARED_REAL_CONTEXT: bool = cfg!(target_os = "emscripten");

/// For [SHARED_REAL_CONTEXT]: the instance that was last made current (0 if
/// none), and the saved state of the instances that aren't.
struct SharedContext {
    current: usize,
    next_id: usize,
    /// (instance, its state). There are only ever a few instances.
    saved: Vec<(usize, RealState)>,
    /// What a new context's viewport and scissor box should be.
    initial_viewport: Option<[GLint; 4]>,
    functions_loaded: bool,
}
static SHARED_CONTEXT: Mutex<SharedContext> = Mutex::new(SharedContext {
    current: 0,
    next_id: 1,
    saved: Vec::new(),
    initial_viewport: None,
    functions_loaded: false,
});

/// The state of the real context that this layer doesn't keep track of
/// itself, because it just passes the calls that set it through.
///
/// TODO: This is read back with `glGet` on every switch, which is simple but
/// not fast. Tracking it as it is set would avoid that. Stencil state is
/// missing.
#[derive(Clone)]
struct RealState {
    framebuffer: GLint,
    renderbuffer: GLint,
    array_buffer: GLint,
    element_array_buffer: GLint,
    active_texture: GLint,
    textures: [GLint; TEXTURE_UNITS],
    viewport: [GLint; 4],
    scissor_box: [GLint; 4],
    capabilities: [bool; PASSED_THROUGH_CAPABILITIES.len()],
    blend_src: GLint,
    blend_dst: GLint,
    blend_equation: GLint,
    clear_color: [GLfloat; 4],
    clear_depth: GLfloat,
    clear_stencil: GLint,
    color_mask: [GLboolean; 4],
    depth_mask: GLboolean,
    depth_func: GLint,
    depth_range: [GLfloat; 2],
    cull_face_mode: GLint,
    front_face: GLint,
    line_width: GLfloat,
    polygon_offset: [GLfloat; 2],
    unpack_alignment: GLint,
    pack_alignment: GLint,
}
impl RealState {
    /// The state of a context that was just created.
    fn initial(viewport: [GLint; 4]) -> Self {
        let mut capabilities = [false; PASSED_THROUGH_CAPABILITIES.len()];
        for (enabled, &cap) in capabilities.iter_mut().zip(PASSED_THROUGH_CAPABILITIES) {
            *enabled = cap == gles11::DITHER;
        }
        RealState {
            framebuffer: 0,
            renderbuffer: 0,
            array_buffer: 0,
            element_array_buffer: 0,
            active_texture: gl::TEXTURE0 as GLint,
            textures: [0; TEXTURE_UNITS],
            viewport,
            scissor_box: viewport,
            capabilities,
            blend_src: gl::ONE as GLint,
            blend_dst: gl::ZERO as GLint,
            blend_equation: gl::FUNC_ADD as GLint,
            clear_color: [0.0; 4],
            clear_depth: 1.0,
            clear_stencil: 0,
            color_mask: [gl::TRUE; 4],
            depth_mask: gl::TRUE,
            depth_func: gl::LESS as GLint,
            depth_range: [0.0, 1.0],
            cull_face_mode: gl::BACK as GLint,
            front_face: gl::CCW as GLint,
            line_width: 1.0,
            polygon_offset: [0.0, 0.0],
            unpack_alignment: 4,
            pack_alignment: 4,
        }
    }

    /// Read the state of the real context.
    unsafe fn capture() -> Self {
        unsafe fn get_int(pname: GLenum) -> GLint {
            let mut value = 0;
            gl::GetIntegerv(pname, &mut value);
            value
        }
        unsafe fn get_ints4(pname: GLenum) -> [GLint; 4] {
            let mut values = [0; 4];
            gl::GetIntegerv(pname, values.as_mut_ptr());
            values
        }
        unsafe fn get_float(pname: GLenum) -> GLfloat {
            let mut value = 0.0;
            gl::GetFloatv(pname, &mut value);
            value
        }

        let active_texture = get_int(gl::ACTIVE_TEXTURE);
        let mut textures = [0; TEXTURE_UNITS];
        for (unit, texture) in textures.iter_mut().enumerate() {
            gl::ActiveTexture(gl::TEXTURE0 + unit as GLenum);
            *texture = get_int(gl::TEXTURE_BINDING_2D);
        }
        gl::ActiveTexture(active_texture as GLenum);

        let mut capabilities = [false; PASSED_THROUGH_CAPABILITIES.len()];
        for (enabled, &cap) in capabilities.iter_mut().zip(PASSED_THROUGH_CAPABILITIES) {
            *enabled = gl::IsEnabled(cap) != 0;
        }
        let mut clear_color = [0.0; 4];
        gl::GetFloatv(gl::COLOR_CLEAR_VALUE, clear_color.as_mut_ptr());
        let mut color_mask = [0; 4];
        gl::GetBooleanv(gl::COLOR_WRITEMASK, color_mask.as_mut_ptr());
        let mut depth_mask = 0;
        gl::GetBooleanv(gl::DEPTH_WRITEMASK, &mut depth_mask);
        let mut depth_range = [0.0; 2];
        gl::GetFloatv(gl::DEPTH_RANGE, depth_range.as_mut_ptr());

        RealState {
            framebuffer: get_int(gl::FRAMEBUFFER_BINDING),
            renderbuffer: get_int(gl::RENDERBUFFER_BINDING),
            array_buffer: get_int(gl::ARRAY_BUFFER_BINDING),
            element_array_buffer: get_int(gl::ELEMENT_ARRAY_BUFFER_BINDING),
            active_texture,
            textures,
            viewport: get_ints4(gl::VIEWPORT),
            scissor_box: get_ints4(gl::SCISSOR_BOX),
            capabilities,
            blend_src: get_int(gl::BLEND_SRC_RGB),
            blend_dst: get_int(gl::BLEND_DST_RGB),
            blend_equation: get_int(gl::BLEND_EQUATION_RGB),
            clear_color,
            clear_depth: get_float(gl::DEPTH_CLEAR_VALUE),
            clear_stencil: get_int(gl::STENCIL_CLEAR_VALUE),
            color_mask,
            depth_mask,
            depth_func: get_int(gl::DEPTH_FUNC),
            depth_range,
            cull_face_mode: get_int(gl::CULL_FACE_MODE),
            front_face: get_int(gl::FRONT_FACE),
            line_width: get_float(gl::LINE_WIDTH),
            polygon_offset: [
                get_float(gl::POLYGON_OFFSET_FACTOR),
                get_float(gl::POLYGON_OFFSET_UNITS),
            ],
            unpack_alignment: get_int(gl::UNPACK_ALIGNMENT),
            pack_alignment: get_int(gl::PACK_ALIGNMENT),
        }
    }

    /// Put the real context in this state.
    unsafe fn apply(&self) {
        gl::BindFramebuffer(gl::FRAMEBUFFER, self.framebuffer as GLuint);
        gl::BindRenderbuffer(gl::RENDERBUFFER, self.renderbuffer as GLuint);
        gl::BindBuffer(gl::ARRAY_BUFFER, self.array_buffer as GLuint);
        gl::BindBuffer(gl::ELEMENT_ARRAY_BUFFER, self.element_array_buffer as GLuint);
        for (unit, &texture) in self.textures.iter().enumerate() {
            gl::ActiveTexture(gl::TEXTURE0 + unit as GLenum);
            gl::BindTexture(gl::TEXTURE_2D, texture as GLuint);
        }
        gl::ActiveTexture(self.active_texture as GLenum);
        let [x, y, width, height] = self.viewport;
        gl::Viewport(x, y, width, height);
        let [x, y, width, height] = self.scissor_box;
        gl::Scissor(x, y, width, height);
        for (&enabled, &cap) in self.capabilities.iter().zip(PASSED_THROUGH_CAPABILITIES) {
            if enabled {
                gl::Enable(cap);
            } else {
                gl::Disable(cap);
            }
        }
        gl::BlendFunc(self.blend_src as GLenum, self.blend_dst as GLenum);
        gl::BlendEquation(self.blend_equation as GLenum);
        let [r, g, b, a] = self.clear_color;
        gl::ClearColor(r, g, b, a);
        gl::ClearDepthf(self.clear_depth);
        gl::ClearStencil(self.clear_stencil);
        let [r, g, b, a] = self.color_mask;
        gl::ColorMask(r, g, b, a);
        gl::DepthMask(self.depth_mask);
        gl::DepthFunc(self.depth_func as GLenum);
        gl::DepthRangef(self.depth_range[0], self.depth_range[1]);
        gl::CullFace(self.cull_face_mode as GLenum);
        gl::FrontFace(self.front_face as GLenum);
        gl::LineWidth(self.line_width);
        gl::PolygonOffset(self.polygon_offset[0], self.polygon_offset[1]);
        gl::PixelStorei(gl::UNPACK_ALIGNMENT, self.unpack_alignment);
        gl::PixelStorei(gl::PACK_ALIGNMENT, self.pack_alignment);
    }
}

/// A 4-by-4 matrix in OpenGL's layout (column-major).
type Mat4 = [GLfloat; 16];

const IDENTITY: Mat4 = [
    1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
];

/// `a` × `b`: the transformation that applies `b` first, then `a`.
fn mat4_multiply(a: &Mat4, b: &Mat4) -> Mat4 {
    let mut result = [0.0; 16];
    for column in 0..4 {
        for row in 0..4 {
            result[column * 4 + row] = (0..4)
                .map(|k| a[k * 4 + row] * b[column * 4 + k])
                .sum();
        }
    }
    result
}

#[derive(Copy, Clone)]
struct ArrayState {
    enabled: bool,
    size: GLint,
    type_: GLenum,
    stride: GLsizei,
    /// A pointer, or an offset if `buffer` is not zero.
    pointer: *const GLvoid,
    /// The buffer that was bound to `GL_ARRAY_BUFFER` when the pointer was
    /// set.
    buffer: GLuint,
}
impl ArrayState {
    fn new(size: GLint) -> Self {
        ArrayState {
            enabled: false,
            size,
            type_: gles11::FLOAT,
            stride: 0,
            pointer: std::ptr::null(),
            buffer: 0,
        }
    }
}

struct TextureUnit {
    /// Whether `GL_TEXTURE_2D` is enabled.
    enabled: bool,
    /// The texture bound to `GL_TEXTURE_2D`.
    bound_texture: GLuint,
    env_mode: GLenum,
    env_color: [GLfloat; 4],
    matrix_stack: Vec<Mat4>,
    tex_coord_array: ArrayState,
}

struct Program {
    name: GLuint,
    u_mvp: GLint,
    u_tex_matrix: [GLint; TEXTURE_UNITS],
    u_point_size: GLint,
    u_tex_env: [GLint; TEXTURE_UNITS],
    u_tex_env_color: [GLint; TEXTURE_UNITS],
    u_tex_format: [GLint; TEXTURE_UNITS],
    u_alpha_func: GLint,
    u_alpha_ref: GLint,
}

pub struct GLES1OnGLES2 {
    gl_ctx: GLContext,
    /// Identifies this instance for [SHARED_REAL_CONTEXT].
    id: usize,
    /// The variants of the shader program, each created the first time it is
    /// needed. Bit `n` of the index is set if texture unit `n` is in use.
    programs: [Option<Program>; 1 << TEXTURE_UNITS],

    matrix_mode: GLenum,
    modelview_stack: Vec<Mat4>,
    projection_stack: Vec<Mat4>,

    units: [TextureUnit; TEXTURE_UNITS],
    active_texture: usize,
    client_active_texture: usize,

    vertex_array: ArrayState,
    color_array: ArrayState,
    normal_array: ArrayState,
    current_color: [GLfloat; 4],
    current_normal: [GLfloat; 3],
    /// The buffer bound to `GL_ARRAY_BUFFER`.
    array_buffer: GLuint,

    alpha_test: bool,
    alpha_func: GLenum,
    alpha_ref: GLfloat,
    point_size: GLfloat,
    shade_model: GLenum,
    fog_params: HashMap<GLenum, [GLfloat; 4]>,
    ignored_capabilities: HashSet<GLenum>,
}

/// Compile a shader from `prefix` (for `#define`s) followed by `source`.
unsafe fn compile_shader(type_: GLenum, prefix: &str, source: &str) -> GLuint {
    let shader = gl::CreateShader(type_);
    let source_ptrs = [
        prefix.as_ptr() as *const GLchar,
        source.as_ptr() as *const GLchar,
    ];
    let source_lens = [prefix.len() as GLint, source.len() as GLint];
    gl::ShaderSource(shader, 2, source_ptrs.as_ptr(), source_lens.as_ptr());
    gl::CompileShader(shader);
    let mut status = 0;
    gl::GetShaderiv(shader, gl::COMPILE_STATUS, &mut status);
    if status == 0 {
        let mut log = vec![0u8; 4096];
        let mut log_len = 0;
        gl::GetShaderInfoLog(
            shader,
            log.len() as GLsizei,
            &mut log_len,
            log.as_mut_ptr() as *mut GLchar,
        );
        panic!(
            "GLES1-on-GLES2 shader failed to compile: {}",
            String::from_utf8_lossy(&log[..log_len.max(0) as usize])
        );
    }
    shader
}

impl GLES1OnGLES2 {
    fn matrix_stack(&mut self) -> &mut Vec<Mat4> {
        match self.matrix_mode {
            gles11::MODELVIEW => &mut self.modelview_stack,
            gles11::PROJECTION => &mut self.projection_stack,
            gles11::TEXTURE => &mut self.units[self.active_texture].matrix_stack,
            other => unimplemented!("GLES1-on-GLES2: matrix mode {:#x}", other),
        }
    }
    fn matrix(&mut self) -> &mut Mat4 {
        self.matrix_stack().last_mut().unwrap()
    }
    fn multiply_matrix(&mut self, by: &Mat4) {
        let matrix = self.matrix();
        *matrix = mat4_multiply(matrix, by);
    }

    /// Look up one of the arrays by its capability name, e.g.
    /// `GL_VERTEX_ARRAY`.
    fn array_state(&mut self, array: GLenum) -> Option<&mut ArrayState> {
        match array {
            gles11::VERTEX_ARRAY => Some(&mut self.vertex_array),
            gles11::COLOR_ARRAY => Some(&mut self.color_array),
            gles11::NORMAL_ARRAY => Some(&mut self.normal_array),
            gles11::TEXTURE_COORD_ARRAY => {
                Some(&mut self.units[self.client_active_texture].tex_coord_array)
            }
            _ => None,
        }
    }

    /// Get the variant of the shader program for a combination of texture
    /// units in use (see [Self::programs]), creating it if necessary.
    unsafe fn ensure_program(&mut self, variant: usize) -> &Program {
        if self.programs[variant].is_none() {
            let mut defines = String::new();
            for unit in 0..TEXTURE_UNITS {
                if variant & (1 << unit) != 0 {
                    defines.push_str(&format!("#define TEXTURE{unit}\n"));
                }
            }
            let vertex_shader = compile_shader(gl::VERTEX_SHADER, "", VERTEX_SHADER);
            let fragment_shader = compile_shader(gl::FRAGMENT_SHADER, &defines, FRAGMENT_SHADER);
            let name = gl::CreateProgram();
            gl::AttachShader(name, vertex_shader);
            gl::AttachShader(name, fragment_shader);
            for (location, attribute) in [
                (ATTRIB_POSITION, c"a_position"),
                (ATTRIB_COLOR, c"a_color"),
                (ATTRIB_TEX_COORD[0], c"a_tex_coord0"),
                (ATTRIB_TEX_COORD[1], c"a_tex_coord1"),
            ] {
                gl::BindAttribLocation(name, location, attribute.as_ptr());
            }
            gl::LinkProgram(name);
            let mut status = 0;
            gl::GetProgramiv(name, gl::LINK_STATUS, &mut status);
            if status == 0 {
                let mut log = vec![0u8; 4096];
                let mut log_len = 0;
                gl::GetProgramInfoLog(
                    name,
                    log.len() as GLsizei,
                    &mut log_len,
                    log.as_mut_ptr() as *mut GLchar,
                );
                panic!(
                    "GLES1-on-GLES2 shader program failed to link: {}",
                    String::from_utf8_lossy(&log[..log_len.max(0) as usize])
                );
            }
            gl::DeleteShader(vertex_shader);
            gl::DeleteShader(fragment_shader);

            let uniform = |uniform: &CStr| gl::GetUniformLocation(name, uniform.as_ptr());
            let program = Program {
                name,
                u_mvp: uniform(c"u_mvp"),
                u_tex_matrix: [uniform(c"u_tex_matrix0"), uniform(c"u_tex_matrix1")],
                u_point_size: uniform(c"u_point_size"),
                u_tex_env: [uniform(c"u_tex_env0"), uniform(c"u_tex_env1")],
                u_tex_env_color: [uniform(c"u_tex_env_color0"), uniform(c"u_tex_env_color1")],
                u_tex_format: [uniform(c"u_tex_format0"), uniform(c"u_tex_format1")],
                u_alpha_func: uniform(c"u_alpha_func"),
                u_alpha_ref: uniform(c"u_alpha_ref"),
            };
            // Setting a uniform that doesn't exist in this variant (location
            // -1) does nothing.
            gl::UseProgram(name);
            gl::Uniform1i(uniform(c"u_texture0"), 0);
            gl::Uniform1i(uniform(c"u_texture1"), 1);
            self.programs[variant] = Some(program);
        }
        self.programs[variant].as_ref().unwrap()
    }

    /// Set up the shader program, its uniforms and the vertex attributes
    /// according to the current OpenGL ES 1.1 state. Returns `false` if there
    /// is nothing to draw.
    unsafe fn prepare_draw(&mut self) -> bool {
        if !self.vertex_array.enabled {
            return false;
        }
        let mut variant = 0;
        for (i, unit) in self.units.iter().enumerate() {
            if unit.enabled && unit.bound_texture != 0 {
                variant |= 1 << i;
            }
        }
        self.ensure_program(variant);
        let program = self.programs[variant].as_ref().unwrap();
        gl::UseProgram(program.name);

        let mvp = mat4_multiply(
            self.projection_stack.last().unwrap(),
            self.modelview_stack.last().unwrap(),
        );
        gl::UniformMatrix4fv(program.u_mvp, 1, gl::FALSE, mvp.as_ptr());
        gl::Uniform1f(program.u_point_size, self.point_size);

        for (i, unit) in self.units.iter().enumerate() {
            let textured = unit.enabled && unit.bound_texture != 0;
            let env_mode = match unit.env_mode {
                _ if !textured => 0,
                gles11::MODULATE => 1,
                gles11::REPLACE => 2,
                gles11::DECAL => 3,
                gles11::BLEND => 4,
                gles11::ADD => 5,
                // GL_COMBINE is not implemented; this is its default
                // configuration.
                _ => 1,
            };
            gl::Uniform1i(program.u_tex_env[i], env_mode);
            if !textured {
                continue;
            }
            let format = with_texture_info(|info| info.formats.get(&unit.bound_texture).copied());
            let (has_color, has_alpha) = match format {
                Some(gles11::ALPHA) => (false, true),
                Some(gles11::RGB) | Some(gles11::LUMINANCE) => (true, false),
                // RGBA, LUMINANCE_ALPHA, or unknown
                _ => (true, true),
            };
            gl::Uniform2f(
                program.u_tex_format[i],
                has_color as u8 as GLfloat,
                has_alpha as u8 as GLfloat,
            );
            gl::Uniform4fv(program.u_tex_env_color[i], 1, unit.env_color.as_ptr());
            gl::UniformMatrix4fv(
                program.u_tex_matrix[i],
                1,
                gl::FALSE,
                unit.matrix_stack.last().unwrap().as_ptr(),
            );
        }

        let alpha_func = match self.alpha_func {
            _ if !self.alpha_test => 0,
            gles11::ALWAYS => 0,
            func => (func - gles11::NEVER + 1) as GLint,
        };
        gl::Uniform1i(program.u_alpha_func, alpha_func);
        gl::Uniform1f(program.u_alpha_ref, self.alpha_ref);

        // Integer colours are in the range of the type and get scaled to
        // 0..1, but integer positions and texture coordinates are used as-is.
        unsafe fn set_attribute(location: GLuint, array: &ArrayState, normalized: bool) {
            gl::BindBuffer(gl::ARRAY_BUFFER, array.buffer);
            gl::EnableVertexAttribArray(location);
            gl::VertexAttribPointer(
                location,
                array.size,
                array.type_,
                (normalized && array.type_ != gles11::FLOAT && array.type_ != gles11::FIXED) as u8,
                array.stride,
                array.pointer,
            );
        }

        set_attribute(ATTRIB_POSITION, &self.vertex_array, false);
        if self.color_array.enabled {
            set_attribute(ATTRIB_COLOR, &self.color_array, true);
        } else {
            gl::DisableVertexAttribArray(ATTRIB_COLOR);
            let [r, g, b, a] = self.current_color;
            gl::VertexAttrib4f(ATTRIB_COLOR, r, g, b, a);
        }
        for (i, unit) in self.units.iter().enumerate() {
            if unit.tex_coord_array.enabled {
                set_attribute(ATTRIB_TEX_COORD[i], &unit.tex_coord_array, false);
            } else {
                gl::DisableVertexAttribArray(ATTRIB_TEX_COORD[i]);
                gl::VertexAttrib4f(ATTRIB_TEX_COORD[i], 0.0, 0.0, 0.0, 1.0);
            }
        }
        gl::BindBuffer(gl::ARRAY_BUFFER, self.array_buffer);
        true
    }

    /// Regenerate mipmaps after the base level of the bound texture changed,
    /// if the texture has `GL_GENERATE_MIPMAP` set.
    unsafe fn after_texture_upload(&mut self, level: GLint) {
        let texture = self.units[self.active_texture].bound_texture;
        if level == 0 && with_texture_info(|info| info.generate_mipmap.contains(&texture)) {
            gl::GenerateMipmap(gl::TEXTURE_2D);
        }
    }

    fn set_texture_format(&mut self, level: GLint, format: GLenum) {
        if level == 0 {
            let texture = self.units[self.active_texture].bound_texture;
            with_texture_info(|info| info.formats.insert(texture, format));
        }
    }
}

impl Drop for GLES1OnGLES2 {
    fn drop(&mut self) {
        let mut shared = SHARED_CONTEXT.lock().unwrap();
        let own_id = self.id;
        shared.saved.retain(|&(id, _)| id != own_id);
        if shared.current == self.id {
            // Nothing to save, and whatever comes next starts afresh.
            shared.current = 0;
        }
    }
}

/// Convert BGRA pixels to RGBA.
unsafe fn bgra_to_rgba(pixels: *const GLvoid, width: GLsizei, height: GLsizei) -> Vec<u8> {
    let len = width as usize * height as usize * 4;
    let mut converted = std::slice::from_raw_parts(pixels.cast::<u8>(), len).to_vec();
    for pixel in converted.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
    converted
}

impl GLES for GLES1OnGLES2 {
    fn description() -> &'static str {
        "OpenGL ES 1.1 via touchHLE GLES1-on-GLES2 layer"
    }

    fn new(window: &mut Window) -> Result<Self, String> {
        let new_unit = || TextureUnit {
            enabled: false,
            bound_texture: 0,
            env_mode: gles11::MODULATE,
            env_color: [0.0; 4],
            matrix_stack: vec![IDENTITY],
            tex_coord_array: ArrayState::new(4),
        };
        Ok(Self {
            gl_ctx: window.create_gl_context(GLVersion::GLES20)?,
            id: {
                let mut shared = SHARED_CONTEXT.lock().unwrap();
                shared.next_id += 1;
                shared.next_id - 1
            },
            programs: Default::default(),
            matrix_mode: gles11::MODELVIEW,
            modelview_stack: vec![IDENTITY],
            projection_stack: vec![IDENTITY],
            units: [new_unit(), new_unit()],
            active_texture: 0,
            client_active_texture: 0,
            vertex_array: ArrayState::new(4),
            color_array: ArrayState::new(4),
            normal_array: ArrayState::new(3),
            current_color: [1.0; 4],
            current_normal: [0.0, 0.0, 1.0],
            array_buffer: 0,
            alpha_test: false,
            alpha_func: gles11::ALWAYS,
            alpha_ref: 0.0,
            point_size: 1.0,
            shade_model: gles11::SMOOTH,
            fog_params: HashMap::new(),
            ignored_capabilities: HashSet::new(),
        })
    }

    fn make_current(&self, window: &Window) {
        unsafe { window.make_gl_context_current(&self.gl_ctx) };
        if !SHARED_REAL_CONTEXT {
            gl::load_with(|s| window.gl_get_proc_address(s));
            return;
        }

        let mut shared = SHARED_CONTEXT.lock().unwrap();
        // There is only one context, so the functions never change, and
        // looking them all up again is slow.
        if !shared.functions_loaded {
            gl::load_with(|s| window.gl_get_proc_address(s));
            shared.functions_loaded = true;
        }
        let previous = std::mem::replace(&mut shared.current, self.id);
        if previous == self.id {
            return;
        }
        unsafe {
            let initial_viewport = *shared.initial_viewport.get_or_insert_with(|| {
                let mut viewport = [0; 4];
                gl::GetIntegerv(gl::VIEWPORT, viewport.as_mut_ptr());
                viewport
            });
            if previous != 0 {
                shared.saved.retain(|&(id, _)| id != previous);
                shared.saved.push((previous, RealState::capture()));
            }
            let state = match shared.saved.iter().position(|&(id, _)| id == self.id) {
                Some(index) => shared.saved.swap_remove(index).1,
                None => RealState::initial(initial_viewport),
            };
            state.apply();
        }
    }

    unsafe fn driver_description(&self) -> String {
        let version = CStr::from_ptr(gl::GetString(gl::VERSION) as *const _);
        let vendor = CStr::from_ptr(gl::GetString(gl::VENDOR) as *const _);
        let renderer = CStr::from_ptr(gl::GetString(gl::RENDERER) as *const _);
        format!(
            "{} / {} / {}",
            version.to_string_lossy(),
            vendor.to_string_lossy(),
            renderer.to_string_lossy()
        )
    }

    // Generic state manipulation
    unsafe fn GetError(&mut self) -> GLenum {
        gl::GetError()
    }
    unsafe fn Enable(&mut self, cap: GLenum) {
        if let Some(array) = self.array_state(cap) {
            array.enabled = true;
        } else if cap == gles11::TEXTURE_2D {
            self.units[self.active_texture].enabled = true;
        } else if cap == gles11::ALPHA_TEST {
            self.alpha_test = true;
        } else if PASSED_THROUGH_CAPABILITIES.contains(&cap) {
            gl::Enable(cap);
        } else if IGNORED_CAPABILITIES.contains(&cap) {
            if cap == gles11::LIGHTING {
                log_once!("GLES1-on-GLES2: lighting is not implemented, things will be drawn unlit");
            } else if cap == gles11::FOG {
                log_once!("GLES1-on-GLES2: fog is not implemented, things will be drawn without it");
            }
            self.ignored_capabilities.insert(cap);
        } else {
            unimplemented!("GLES1-on-GLES2: glEnable({:#x})", cap);
        }
    }
    unsafe fn IsEnabled(&mut self, cap: GLenum) -> GLboolean {
        if let Some(array) = self.array_state(cap) {
            array.enabled as GLboolean
        } else if cap == gles11::TEXTURE_2D {
            self.units[self.active_texture].enabled as GLboolean
        } else if cap == gles11::ALPHA_TEST {
            self.alpha_test as GLboolean
        } else if PASSED_THROUGH_CAPABILITIES.contains(&cap) {
            gl::IsEnabled(cap)
        } else if IGNORED_CAPABILITIES.contains(&cap) {
            self.ignored_capabilities.contains(&cap) as GLboolean
        } else {
            unimplemented!("GLES1-on-GLES2: glIsEnabled({:#x})", cap);
        }
    }
    unsafe fn Disable(&mut self, cap: GLenum) {
        if let Some(array) = self.array_state(cap) {
            array.enabled = false;
        } else if cap == gles11::TEXTURE_2D {
            self.units[self.active_texture].enabled = false;
        } else if cap == gles11::ALPHA_TEST {
            self.alpha_test = false;
        } else if PASSED_THROUGH_CAPABILITIES.contains(&cap) {
            gl::Disable(cap);
        } else if IGNORED_CAPABILITIES.contains(&cap) {
            self.ignored_capabilities.remove(&cap);
        } else {
            unimplemented!("GLES1-on-GLES2: glDisable({:#x})", cap);
        }
    }
    unsafe fn ClientActiveTexture(&mut self, texture: GLenum) {
        let unit = (texture - gles11::TEXTURE0) as usize;
        assert!(unit < TEXTURE_UNITS);
        self.client_active_texture = unit;
    }
    unsafe fn EnableClientState(&mut self, array: GLenum) {
        match self.array_state(array) {
            Some(array) => array.enabled = true,
            None => unimplemented!("GLES1-on-GLES2: glEnableClientState({:#x})", array),
        }
    }
    unsafe fn DisableClientState(&mut self, array: GLenum) {
        match self.array_state(array) {
            Some(array) => array.enabled = false,
            // Disabling something that can't be enabled is harmless.
            None => (),
        }
    }
    unsafe fn GetBooleanv(&mut self, pname: GLenum, params: *mut GLboolean) {
        if self.array_state(pname).is_some()
            || pname == gles11::TEXTURE_2D
            || pname == gles11::ALPHA_TEST
            || PASSED_THROUGH_CAPABILITIES.contains(&pname)
            || IGNORED_CAPABILITIES.contains(&pname)
        {
            params.write(self.IsEnabled(pname));
        } else {
            gl::GetBooleanv(pname, params);
        }
    }
    unsafe fn GetFloatv(&mut self, pname: GLenum, params: *mut GLfloat) {
        let values: &[GLfloat] = match pname {
            gles11::CURRENT_COLOR => &self.current_color,
            gles11::CURRENT_NORMAL => &self.current_normal,
            gles11::MODELVIEW_MATRIX => self.modelview_stack.last().unwrap(),
            gles11::PROJECTION_MATRIX => self.projection_stack.last().unwrap(),
            gles11::TEXTURE_MATRIX => self.units[self.active_texture].matrix_stack.last().unwrap(),
            gles11::ALPHA_TEST_REF => std::slice::from_ref(&self.alpha_ref),
            gles11::POINT_SIZE => std::slice::from_ref(&self.point_size),
            gles11::FOG_START | gles11::FOG_END | gles11::FOG_DENSITY => {
                let default = match pname {
                    gles11::FOG_START => 0.0,
                    _ => 1.0,
                };
                let value = self.fog_params.get(&pname).map_or(default, |v| v[0]);
                params.write(value);
                return;
            }
            _ => {
                gl::GetFloatv(pname, params);
                return;
            }
        };
        std::ptr::copy_nonoverlapping(values.as_ptr(), params, values.len());
    }
    unsafe fn GetIntegerv(&mut self, pname: GLenum, params: *mut GLint) {
        let active_texture = self.active_texture;
        let client_active_texture = self.client_active_texture;
        let value: GLint = match pname {
            gles11::MATRIX_MODE => self.matrix_mode as GLint,
            gles11::CLIENT_ACTIVE_TEXTURE => (gles11::TEXTURE0 as usize + client_active_texture) as GLint,
            gles11::MAX_TEXTURE_UNITS => TEXTURE_UNITS as GLint,
            gles11::MAX_MODELVIEW_STACK_DEPTH => 32,
            gles11::MAX_PROJECTION_STACK_DEPTH | gles11::MAX_TEXTURE_STACK_DEPTH => 4,
            gles11::MODELVIEW_STACK_DEPTH => self.modelview_stack.len() as GLint,
            gles11::PROJECTION_STACK_DEPTH => self.projection_stack.len() as GLint,
            gles11::TEXTURE_STACK_DEPTH => self.units[active_texture].matrix_stack.len() as GLint,
            gles11::MAX_LIGHTS => 8,
            gles11::MAX_CLIP_PLANES => 6,
            gles11::ALPHA_TEST_FUNC => self.alpha_func as GLint,
            gles11::SHADE_MODEL => self.shade_model as GLint,
            gles11::VERTEX_ARRAY_SIZE => self.vertex_array.size,
            gles11::VERTEX_ARRAY_TYPE => self.vertex_array.type_ as GLint,
            gles11::VERTEX_ARRAY_STRIDE => self.vertex_array.stride,
            gles11::VERTEX_ARRAY_BUFFER_BINDING => self.vertex_array.buffer as GLint,
            gles11::COLOR_ARRAY_SIZE => self.color_array.size,
            gles11::COLOR_ARRAY_TYPE => self.color_array.type_ as GLint,
            gles11::COLOR_ARRAY_STRIDE => self.color_array.stride,
            gles11::COLOR_ARRAY_BUFFER_BINDING => self.color_array.buffer as GLint,
            gles11::NORMAL_ARRAY_TYPE => self.normal_array.type_ as GLint,
            gles11::NORMAL_ARRAY_STRIDE => self.normal_array.stride,
            gles11::NORMAL_ARRAY_BUFFER_BINDING => self.normal_array.buffer as GLint,
            gles11::TEXTURE_COORD_ARRAY_SIZE => self.units[client_active_texture].tex_coord_array.size,
            gles11::TEXTURE_COORD_ARRAY_TYPE => {
                self.units[client_active_texture].tex_coord_array.type_ as GLint
            }
            gles11::TEXTURE_COORD_ARRAY_STRIDE => {
                self.units[client_active_texture].tex_coord_array.stride
            }
            gles11::TEXTURE_COORD_ARRAY_BUFFER_BINDING => {
                self.units[client_active_texture].tex_coord_array.buffer as GLint
            }
            // OpenGL ES 2.0 has separate RGB and alpha blend functions, and
            // no name for both at once.
            gles11::BLEND_SRC => {
                gl::GetIntegerv(gl::BLEND_SRC_RGB, params);
                return;
            }
            gles11::BLEND_DST => {
                gl::GetIntegerv(gl::BLEND_DST_RGB, params);
                return;
            }
            _ => {
                if self.array_state(pname).is_some()
                    || pname == gles11::TEXTURE_2D
                    || pname == gles11::ALPHA_TEST
                    || IGNORED_CAPABILITIES.contains(&pname)
                {
                    self.IsEnabled(pname) as GLint
                } else {
                    gl::GetIntegerv(pname, params);
                    return;
                }
            }
        };
        params.write(value);
    }
    unsafe fn GetTexEnviv(&mut self, target: GLenum, pname: GLenum, params: *mut GLint) {
        match (target, pname) {
            (gles11::TEXTURE_ENV, gles11::TEXTURE_ENV_MODE) => {
                params.write(self.units[self.active_texture].env_mode as GLint)
            }
            _ => unimplemented!("GLES1-on-GLES2: glGetTexEnviv({:#x}, {:#x})", target, pname),
        }
    }
    unsafe fn GetTexEnvfv(&mut self, target: GLenum, pname: GLenum, params: *mut GLfloat) {
        match (target, pname) {
            (gles11::TEXTURE_ENV, gles11::TEXTURE_ENV_MODE) => {
                params.write(self.units[self.active_texture].env_mode as GLfloat)
            }
            (gles11::TEXTURE_ENV, gles11::TEXTURE_ENV_COLOR) => {
                let color = self.units[self.active_texture].env_color;
                std::ptr::copy_nonoverlapping(color.as_ptr(), params, 4);
            }
            _ => unimplemented!("GLES1-on-GLES2: glGetTexEnvfv({:#x}, {:#x})", target, pname),
        }
    }
    unsafe fn GetPointerv(&mut self, pname: GLenum, params: *mut *const GLvoid) {
        let array = match pname {
            gles11::VERTEX_ARRAY_POINTER => &self.vertex_array,
            gles11::COLOR_ARRAY_POINTER => &self.color_array,
            gles11::NORMAL_ARRAY_POINTER => &self.normal_array,
            gles11::TEXTURE_COORD_ARRAY_POINTER => {
                &self.units[self.client_active_texture].tex_coord_array
            }
            _ => unimplemented!("GLES1-on-GLES2: glGetPointerv({:#x})", pname),
        };
        params.write(array.pointer);
    }
    unsafe fn Hint(&mut self, target: GLenum, mode: GLenum) {
        // The other hints are for things that don't exist in OpenGL ES 2.0.
        if target == gles11::GENERATE_MIPMAP_HINT {
            gl::Hint(target, mode);
        }
    }
    unsafe fn Finish(&mut self) {
        gl::Finish();
    }
    unsafe fn Flush(&mut self) {
        gl::Flush();
    }
    unsafe fn GetString(&mut self, name: GLenum) -> *const GLubyte {
        gl::GetString(name)
    }

    // Other state manipulation
    unsafe fn AlphaFunc(&mut self, func: GLenum, ref_: GLclampf) {
        assert!((gles11::NEVER..=gles11::ALWAYS).contains(&func));
        self.alpha_func = func;
        self.alpha_ref = ref_.clamp(0.0, 1.0);
    }
    unsafe fn AlphaFuncx(&mut self, func: GLenum, ref_: GLclampx) {
        self.AlphaFunc(func, fixed_to_float(ref_))
    }
    unsafe fn BlendFunc(&mut self, sfactor: GLenum, dfactor: GLenum) {
        gl::BlendFunc(sfactor, dfactor);
    }
    unsafe fn BlendEquationOES(&mut self, mode: GLenum) {
        gl::BlendEquation(mode);
    }
    unsafe fn ColorMask(
        &mut self,
        red: GLboolean,
        green: GLboolean,
        blue: GLboolean,
        alpha: GLboolean,
    ) {
        gl::ColorMask(red, green, blue, alpha);
    }
    unsafe fn ClipPlanef(&mut self, _plane: GLenum, _equation: *const GLfloat) {
        log_once!("GLES1-on-GLES2: user clip planes are not implemented");
    }
    unsafe fn ClipPlanex(&mut self, _plane: GLenum, _equation: *const GLfixed) {
        log_once!("GLES1-on-GLES2: user clip planes are not implemented");
    }
    unsafe fn CullFace(&mut self, mode: GLenum) {
        gl::CullFace(mode);
    }
    unsafe fn DepthFunc(&mut self, func: GLenum) {
        gl::DepthFunc(func);
    }
    unsafe fn DepthMask(&mut self, flag: GLboolean) {
        gl::DepthMask(flag);
    }
    unsafe fn DepthRangef(&mut self, near: GLclampf, far: GLclampf) {
        gl::DepthRangef(near, far);
    }
    unsafe fn DepthRangex(&mut self, near: GLclampx, far: GLclampx) {
        gl::DepthRangef(fixed_to_float(near), fixed_to_float(far));
    }
    unsafe fn FrontFace(&mut self, mode: GLenum) {
        gl::FrontFace(mode);
    }
    unsafe fn PolygonOffset(&mut self, factor: GLfloat, units: GLfloat) {
        gl::PolygonOffset(factor, units);
    }
    unsafe fn PolygonOffsetx(&mut self, factor: GLfixed, units: GLfixed) {
        gl::PolygonOffset(fixed_to_float(factor), fixed_to_float(units));
    }
    unsafe fn SampleCoverage(&mut self, value: GLclampf, invert: GLboolean) {
        gl::SampleCoverage(value, invert);
    }
    unsafe fn SampleCoveragex(&mut self, value: GLclampx, invert: GLboolean) {
        gl::SampleCoverage(fixed_to_float(value), invert);
    }
    unsafe fn ShadeModel(&mut self, mode: GLenum) {
        if mode == gles11::FLAT {
            log_once!("GLES1-on-GLES2: flat shading is not implemented, using smooth shading");
        }
        self.shade_model = mode;
    }
    unsafe fn Scissor(&mut self, x: GLint, y: GLint, width: GLsizei, height: GLsizei) {
        gl::Scissor(x, y, width, height);
    }
    unsafe fn Viewport(&mut self, x: GLint, y: GLint, width: GLsizei, height: GLsizei) {
        gl::Viewport(x, y, width, height);
    }
    unsafe fn LineWidth(&mut self, val: GLfloat) {
        gl::LineWidth(val);
    }
    unsafe fn LineWidthx(&mut self, val: GLfixed) {
        gl::LineWidth(fixed_to_float(val));
    }
    unsafe fn StencilFunc(&mut self, func: GLenum, ref_: GLint, mask: GLuint) {
        gl::StencilFunc(func, ref_, mask);
    }
    unsafe fn StencilOp(&mut self, sfail: GLenum, dpfail: GLenum, dppass: GLenum) {
        gl::StencilOp(sfail, dpfail, dppass);
    }
    unsafe fn StencilMask(&mut self, mask: GLuint) {
        gl::StencilMask(mask);
    }

    // Points
    unsafe fn PointSize(&mut self, size: GLfloat) {
        self.point_size = size;
    }
    unsafe fn PointSizex(&mut self, size: GLfixed) {
        self.point_size = fixed_to_float(size);
    }
    unsafe fn PointParameterf(&mut self, _pname: GLenum, _param: GLfloat) {
        log_once!("GLES1-on-GLES2: point parameters are not implemented");
    }
    unsafe fn PointParameterx(&mut self, _pname: GLenum, _param: GLfixed) {
        log_once!("GLES1-on-GLES2: point parameters are not implemented");
    }
    unsafe fn PointParameterfv(&mut self, _pname: GLenum, _params: *const GLfloat) {
        log_once!("GLES1-on-GLES2: point parameters are not implemented");
    }
    unsafe fn PointParameterxv(&mut self, _pname: GLenum, _params: *const GLfixed) {
        log_once!("GLES1-on-GLES2: point parameters are not implemented");
    }

    // Lighting and materials. None of this is implemented (see the module
    // documentation). The fog parameters are remembered because they can be
    // queried.
    unsafe fn Fogf(&mut self, pname: GLenum, param: GLfloat) {
        self.fog_params.insert(pname, [param, 0.0, 0.0, 0.0]);
    }
    unsafe fn Fogx(&mut self, pname: GLenum, param: GLfixed) {
        // The fog mode is an enum rather than a fixed-point number.
        let param = if pname == gles11::FOG_MODE {
            param as GLfloat
        } else {
            fixed_to_float(param)
        };
        self.Fogf(pname, param)
    }
    unsafe fn Fogfv(&mut self, pname: GLenum, params: *const GLfloat) {
        let mut values = [0.0; 4];
        let count = if pname == gles11::FOG_COLOR { 4 } else { 1 };
        std::ptr::copy_nonoverlapping(params, values.as_mut_ptr(), count);
        self.fog_params.insert(pname, values);
    }
    unsafe fn Fogxv(&mut self, pname: GLenum, params: *const GLfixed) {
        if pname == gles11::FOG_COLOR {
            let mut values = [0.0; 4];
            for (i, value) in values.iter_mut().enumerate() {
                *value = fixed_to_float(params.add(i).read_unaligned());
            }
            self.fog_params.insert(pname, values);
        } else {
            self.Fogx(pname, params.read_unaligned())
        }
    }
    unsafe fn Lightf(&mut self, _light: GLenum, _pname: GLenum, _param: GLfloat) {}
    unsafe fn Lightx(&mut self, _light: GLenum, _pname: GLenum, _param: GLfixed) {}
    unsafe fn Lightfv(&mut self, _light: GLenum, _pname: GLenum, _params: *const GLfloat) {}
    unsafe fn Lightxv(&mut self, _light: GLenum, _pname: GLenum, _params: *const GLfixed) {}
    unsafe fn LightModelf(&mut self, _pname: GLenum, _param: GLfloat) {}
    unsafe fn LightModelx(&mut self, _pname: GLenum, _param: GLfixed) {}
    unsafe fn LightModelfv(&mut self, _pname: GLenum, _params: *const GLfloat) {}
    unsafe fn LightModelxv(&mut self, _pname: GLenum, _params: *const GLfixed) {}
    unsafe fn Materialf(&mut self, _face: GLenum, _pname: GLenum, _param: GLfloat) {}
    unsafe fn Materialx(&mut self, _face: GLenum, _pname: GLenum, _param: GLfixed) {}
    unsafe fn Materialfv(&mut self, _face: GLenum, _pname: GLenum, _params: *const GLfloat) {}
    unsafe fn Materialxv(&mut self, _face: GLenum, _pname: GLenum, _params: *const GLfixed) {}

    // Buffers
    unsafe fn IsBuffer(&mut self, buffer: GLuint) -> GLboolean {
        gl::IsBuffer(buffer)
    }
    unsafe fn GenBuffers(&mut self, n: GLsizei, buffers: *mut GLuint) {
        gl::GenBuffers(n, buffers);
    }
    unsafe fn DeleteBuffers(&mut self, n: GLsizei, buffers: *const GLuint) {
        // Deleting a bound buffer unbinds it.
        for i in 0..n.max(0) as usize {
            if buffers.add(i).read_unaligned() == self.array_buffer {
                self.array_buffer = 0;
            }
        }
        gl::DeleteBuffers(n, buffers);
    }
    unsafe fn BindBuffer(&mut self, target: GLenum, buffer: GLuint) {
        if target == gles11::ARRAY_BUFFER {
            self.array_buffer = buffer;
        }
        gl::BindBuffer(target, buffer);
    }
    unsafe fn BufferData(
        &mut self,
        target: GLenum,
        size: GLsizeiptr,
        data: *const GLvoid,
        usage: GLenum,
    ) {
        gl::BufferData(target, size, data, usage);
    }
    unsafe fn BufferSubData(
        &mut self,
        target: GLenum,
        offset: GLintptr,
        size: GLsizeiptr,
        data: *const GLvoid,
    ) {
        gl::BufferSubData(target, offset, size, data);
    }

    // Non-pointers
    unsafe fn Color4f(&mut self, red: GLfloat, green: GLfloat, blue: GLfloat, alpha: GLfloat) {
        self.current_color = [red, green, blue, alpha];
    }
    unsafe fn Color4x(&mut self, red: GLfixed, green: GLfixed, blue: GLfixed, alpha: GLfixed) {
        self.current_color = [red, green, blue, alpha].map(fixed_to_float);
    }
    unsafe fn Color4ub(&mut self, red: GLubyte, green: GLubyte, blue: GLubyte, alpha: GLubyte) {
        self.current_color = [red, green, blue, alpha].map(|c| GLfloat::from(c) / 255.0);
    }
    unsafe fn Normal3f(&mut self, nx: GLfloat, ny: GLfloat, nz: GLfloat) {
        self.current_normal = [nx, ny, nz];
    }
    unsafe fn Normal3x(&mut self, nx: GLfixed, ny: GLfixed, nz: GLfixed) {
        self.current_normal = [nx, ny, nz].map(fixed_to_float);
    }

    // Pointers
    unsafe fn ColorPointer(
        &mut self,
        size: GLint,
        type_: GLenum,
        stride: GLsizei,
        pointer: *const GLvoid,
    ) {
        let buffer = self.array_buffer;
        self.color_array = ArrayState {
            size,
            type_,
            stride,
            pointer,
            buffer,
            ..self.color_array
        };
    }
    unsafe fn NormalPointer(&mut self, type_: GLenum, stride: GLsizei, pointer: *const GLvoid) {
        let buffer = self.array_buffer;
        self.normal_array = ArrayState {
            size: 3,
            type_,
            stride,
            pointer,
            buffer,
            ..self.normal_array
        };
    }
    unsafe fn TexCoordPointer(
        &mut self,
        size: GLint,
        type_: GLenum,
        stride: GLsizei,
        pointer: *const GLvoid,
    ) {
        let buffer = self.array_buffer;
        let array = &mut self.units[self.client_active_texture].tex_coord_array;
        *array = ArrayState {
            size,
            type_,
            stride,
            pointer,
            buffer,
            ..*array
        };
    }
    unsafe fn VertexPointer(
        &mut self,
        size: GLint,
        type_: GLenum,
        stride: GLsizei,
        pointer: *const GLvoid,
    ) {
        let buffer = self.array_buffer;
        self.vertex_array = ArrayState {
            size,
            type_,
            stride,
            pointer,
            buffer,
            ..self.vertex_array
        };
    }

    // Drawing
    unsafe fn DrawArrays(&mut self, mode: GLenum, first: GLint, count: GLsizei) {
        if self.prepare_draw() {
            gl::DrawArrays(mode, first, count);
        }
    }
    unsafe fn DrawElements(
        &mut self,
        mode: GLenum,
        count: GLsizei,
        type_: GLenum,
        indices: *const GLvoid,
    ) {
        if self.prepare_draw() {
            gl::DrawElements(mode, count, type_, indices);
        }
    }

    // Clearing
    unsafe fn Clear(&mut self, mask: GLbitfield) {
        gl::Clear(mask);
    }
    unsafe fn ClearColor(
        &mut self,
        red: GLclampf,
        green: GLclampf,
        blue: GLclampf,
        alpha: GLclampf,
    ) {
        gl::ClearColor(red, green, blue, alpha);
    }
    unsafe fn ClearColorx(
        &mut self,
        red: GLclampx,
        green: GLclampx,
        blue: GLclampx,
        alpha: GLclampx,
    ) {
        gl::ClearColor(
            fixed_to_float(red),
            fixed_to_float(green),
            fixed_to_float(blue),
            fixed_to_float(alpha),
        );
    }
    unsafe fn ClearDepthf(&mut self, depth: GLclampf) {
        gl::ClearDepthf(depth);
    }
    unsafe fn ClearDepthx(&mut self, depth: GLclampx) {
        gl::ClearDepthf(fixed_to_float(depth));
    }
    unsafe fn ClearStencil(&mut self, s: GLint) {
        gl::ClearStencil(s);
    }

    // Textures
    unsafe fn PixelStorei(&mut self, pname: GLenum, param: GLint) {
        gl::PixelStorei(pname, param);
    }
    unsafe fn ReadPixels(
        &mut self,
        x: GLint,
        y: GLint,
        width: GLsizei,
        height: GLsizei,
        format: GLenum,
        type_: GLenum,
        pixels: *mut GLvoid,
    ) {
        gl::ReadPixels(x, y, width, height, format, type_, pixels);
    }
    unsafe fn GenTextures(&mut self, n: GLsizei, textures: *mut GLuint) {
        gl::GenTextures(n, textures);
    }
    unsafe fn DeleteTextures(&mut self, n: GLsizei, textures: *const GLuint) {
        for i in 0..n.max(0) as usize {
            let texture = textures.add(i).read_unaligned();
            with_texture_info(|info| {
                info.formats.remove(&texture);
                info.generate_mipmap.remove(&texture);
            });
            // Deleting a bound texture unbinds it.
            for unit in &mut self.units {
                if unit.bound_texture == texture {
                    unit.bound_texture = 0;
                }
            }
        }
        gl::DeleteTextures(n, textures);
    }
    unsafe fn ActiveTexture(&mut self, texture: GLenum) {
        let unit = (texture - gles11::TEXTURE0) as usize;
        assert!(unit < TEXTURE_UNITS);
        self.active_texture = unit;
        gl::ActiveTexture(texture);
    }
    unsafe fn IsTexture(&mut self, texture: GLuint) -> GLboolean {
        gl::IsTexture(texture)
    }
    unsafe fn BindTexture(&mut self, target: GLenum, texture: GLuint) {
        assert!(target == gles11::TEXTURE_2D);
        self.units[self.active_texture].bound_texture = texture;
        gl::BindTexture(target, texture);
    }
    unsafe fn TexParameteri(&mut self, target: GLenum, pname: GLenum, param: GLint) {
        assert!(target == gles11::TEXTURE_2D);
        if pname == gles11::GENERATE_MIPMAP {
            let texture = self.units[self.active_texture].bound_texture;
            with_texture_info(|info| {
                if param != 0 {
                    info.generate_mipmap.insert(texture);
                } else {
                    info.generate_mipmap.remove(&texture);
                }
            });
            return;
        }
        gl::TexParameteri(target, pname, param);
    }
    unsafe fn TexParameterf(&mut self, target: GLenum, pname: GLenum, param: GLfloat) {
        match pname {
            gles11::TEXTURE_MIN_FILTER
            | gles11::TEXTURE_MAG_FILTER
            | gles11::TEXTURE_WRAP_S
            | gles11::TEXTURE_WRAP_T
            | gles11::GENERATE_MIPMAP => self.TexParameteri(target, pname, param as GLint),
            _ => gl::TexParameterf(target, pname, param),
        }
    }
    unsafe fn TexParameterx(&mut self, target: GLenum, pname: GLenum, param: GLfixed) {
        // The enum-valued parameters are passed as-is rather than as
        // fixed-point numbers.
        match pname {
            gles11::TEXTURE_MIN_FILTER
            | gles11::TEXTURE_MAG_FILTER
            | gles11::TEXTURE_WRAP_S
            | gles11::TEXTURE_WRAP_T
            | gles11::GENERATE_MIPMAP => self.TexParameteri(target, pname, param),
            _ => gl::TexParameterf(target, pname, fixed_to_float(param)),
        }
    }
    unsafe fn TexParameteriv(&mut self, target: GLenum, pname: GLenum, params: *const GLint) {
        self.TexParameteri(target, pname, params.read_unaligned())
    }
    unsafe fn TexParameterfv(&mut self, target: GLenum, pname: GLenum, params: *const GLfloat) {
        self.TexParameterf(target, pname, params.read_unaligned())
    }
    unsafe fn TexParameterxv(&mut self, target: GLenum, pname: GLenum, params: *const GLfixed) {
        self.TexParameterx(target, pname, params.read_unaligned())
    }
    unsafe fn TexImage2D(
        &mut self,
        target: GLenum,
        level: GLint,
        internalformat: GLint,
        width: GLsizei,
        height: GLsizei,
        border: GLint,
        format: GLenum,
        type_: GLenum,
        pixels: *const GLvoid,
    ) {
        assert!(target == gles11::TEXTURE_2D);
        assert!(border == 0);
        self.set_texture_format(level, internalformat as GLenum);
        if format == BGRA {
            assert!(type_ == gles11::UNSIGNED_BYTE);
            let converted = (!pixels.is_null()).then(|| bgra_to_rgba(pixels, width, height));
            gl::TexImage2D(
                target,
                level,
                gles11::RGBA as GLint,
                width,
                height,
                border,
                gles11::RGBA,
                type_,
                converted.as_ref().map_or(std::ptr::null(), |c| c.as_ptr().cast()),
            );
        } else {
            // In OpenGL ES 2.0 the internal format must be the same as the
            // format.
            gl::TexImage2D(
                target,
                level,
                format as GLint,
                width,
                height,
                border,
                format,
                type_,
                pixels,
            );
        }
        self.after_texture_upload(level);
    }
    unsafe fn TexSubImage2D(
        &mut self,
        target: GLenum,
        level: GLint,
        xoffset: GLint,
        yoffset: GLint,
        width: GLsizei,
        height: GLsizei,
        format: GLenum,
        type_: GLenum,
        pixels: *const GLvoid,
    ) {
        assert!(target == gles11::TEXTURE_2D);
        if format == BGRA {
            assert!(type_ == gles11::UNSIGNED_BYTE);
            let converted = bgra_to_rgba(pixels, width, height);
            gl::TexSubImage2D(
                target,
                level,
                xoffset,
                yoffset,
                width,
                height,
                gles11::RGBA,
                type_,
                converted.as_ptr().cast(),
            );
        } else {
            gl::TexSubImage2D(
                target, level, xoffset, yoffset, width, height, format, type_, pixels,
            );
        }
        self.after_texture_upload(level);
    }
    unsafe fn CompressedTexImage2D(
        &mut self,
        target: GLenum,
        level: GLint,
        internalformat: GLenum,
        width: GLsizei,
        height: GLsizei,
        border: GLint,
        image_size: GLsizei,
        data: *const GLvoid,
    ) {
        let data = std::slice::from_raw_parts(data.cast::<u8>(), image_size as usize);
        // Neither of the compressed formats OpenGL ES 1.1 apps use is
        // generally available in OpenGL ES 2.0, so both are decoded here.
        if try_decode_pvrtc(
            self,
            target,
            level,
            internalformat,
            width,
            height,
            border,
            data,
        ) {
            log_dbg!("Decoded PVRTC");
        } else if let Some(PalettedTextureFormat {
            index_is_nibble,
            palette_entry_format,
            palette_entry_type,
        }) = PalettedTextureFormat::get_info(internalformat)
        {
            assert!(border == 0);
            let palette_entry_size = match palette_entry_type {
                gles11::UNSIGNED_BYTE => match palette_entry_format {
                    gles11::RGB => 3,
                    gles11::RGBA => 4,
                    _ => unreachable!(),
                },
                _ => 2,
            };
            let palette_size = palette_entry_size * if index_is_nibble { 16 } else { 256 };
            let index_count = width as usize * height as usize;
            // TODO: support multiple miplevels in one image
            assert!(level == 0);
            let (palette, indices) = data.split_at(palette_size);

            let mut decoded = Vec::<u8>::with_capacity(palette_entry_size * index_count);
            for i in 0..index_count {
                let index = if index_is_nibble {
                    (indices[i / 2] >> ((1 - (i % 2)) * 4)) & 0xf
                } else {
                    indices[i]
                } as usize;
                decoded.extend_from_slice(&palette[index * palette_entry_size..][..palette_entry_size]);
            }
            log_dbg!("Decoded paletted texture");
            // The rows of the decoded image are not padded.
            gl::PixelStorei(gl::UNPACK_ALIGNMENT, 1);
            self.TexImage2D(
                target,
                level,
                palette_entry_format as GLint,
                width,
                height,
                border,
                palette_entry_format,
                palette_entry_type,
                decoded.as_ptr().cast(),
            );
            gl::PixelStorei(gl::UNPACK_ALIGNMENT, 4);
        } else {
            unimplemented!("CompressedTexImage2D internalformat: {:#x}", internalformat);
        }
    }
    unsafe fn CopyTexImage2D(
        &mut self,
        target: GLenum,
        level: GLint,
        internalformat: GLenum,
        x: GLint,
        y: GLint,
        width: GLsizei,
        height: GLsizei,
        border: GLint,
    ) {
        assert!(target == gles11::TEXTURE_2D);
        self.set_texture_format(level, internalformat);
        gl::CopyTexImage2D(target, level, internalformat, x, y, width, height, border);
        self.after_texture_upload(level);
    }
    unsafe fn CopyTexSubImage2D(
        &mut self,
        target: GLenum,
        level: GLint,
        xoffset: GLint,
        yoffset: GLint,
        x: GLint,
        y: GLint,
        width: GLsizei,
        height: GLsizei,
    ) {
        gl::CopyTexSubImage2D(target, level, xoffset, yoffset, x, y, width, height);
        self.after_texture_upload(level);
    }
    unsafe fn TexEnvf(&mut self, target: GLenum, pname: GLenum, param: GLfloat) {
        self.TexEnvi(target, pname, param as GLint)
    }
    unsafe fn TexEnvx(&mut self, target: GLenum, pname: GLenum, param: GLfixed) {
        // Everything that can be set with this is an enum, apart from a few
        // GL_COMBINE scale factors, which are not implemented anyway.
        self.TexEnvi(target, pname, param)
    }
    unsafe fn TexEnvi(&mut self, target: GLenum, pname: GLenum, param: GLint) {
        match (target, pname) {
            (gles11::TEXTURE_ENV, gles11::TEXTURE_ENV_MODE) => {
                let mode = param as GLenum;
                if mode == gles11::COMBINE {
                    log_once!("GLES1-on-GLES2: the GL_COMBINE texture environment is not implemented, using GL_MODULATE");
                }
                self.units[self.active_texture].env_mode = mode;
            }
            _ => {
                log_once!("GLES1-on-GLES2: ignoring a texture environment parameter that is not implemented");
            }
        }
    }
    unsafe fn TexEnvfv(&mut self, target: GLenum, pname: GLenum, params: *const GLfloat) {
        match (target, pname) {
            (gles11::TEXTURE_ENV, gles11::TEXTURE_ENV_COLOR) => {
                let color = &mut self.units[self.active_texture].env_color;
                std::ptr::copy_nonoverlapping(params, color.as_mut_ptr(), 4);
            }
            _ => self.TexEnvf(target, pname, params.read_unaligned()),
        }
    }
    unsafe fn TexEnvxv(&mut self, target: GLenum, pname: GLenum, params: *const GLfixed) {
        match (target, pname) {
            (gles11::TEXTURE_ENV, gles11::TEXTURE_ENV_COLOR) => {
                let color = &mut self.units[self.active_texture].env_color;
                for (i, component) in color.iter_mut().enumerate() {
                    *component = fixed_to_float(params.add(i).read_unaligned());
                }
            }
            _ => self.TexEnvx(target, pname, params.read_unaligned()),
        }
    }
    unsafe fn TexEnviv(&mut self, target: GLenum, pname: GLenum, params: *const GLint) {
        match (target, pname) {
            (gles11::TEXTURE_ENV, gles11::TEXTURE_ENV_COLOR) => {
                // Integer colours map the full range of the type to -1..1.
                let color = &mut self.units[self.active_texture].env_color;
                for (i, component) in color.iter_mut().enumerate() {
                    *component = (params.add(i).read_unaligned() as GLfloat) / (i32::MAX as GLfloat);
                }
            }
            _ => self.TexEnvi(target, pname, params.read_unaligned()),
        }
    }

    // Matrix stack operations
    unsafe fn MatrixMode(&mut self, mode: GLenum) {
        assert!(matches!(
            mode,
            gles11::MODELVIEW | gles11::PROJECTION | gles11::TEXTURE
        ));
        self.matrix_mode = mode;
    }
    unsafe fn LoadIdentity(&mut self) {
        *self.matrix() = IDENTITY;
    }
    unsafe fn LoadMatrixf(&mut self, m: *const GLfloat) {
        let mut matrix = [0.0; 16];
        std::ptr::copy_nonoverlapping(m, matrix.as_mut_ptr(), 16);
        *self.matrix() = matrix;
    }
    unsafe fn LoadMatrixx(&mut self, m: *const GLfixed) {
        *self.matrix() = matrix_fixed_to_float(m);
    }
    unsafe fn MultMatrixf(&mut self, m: *const GLfloat) {
        let mut matrix = [0.0; 16];
        std::ptr::copy_nonoverlapping(m, matrix.as_mut_ptr(), 16);
        self.multiply_matrix(&matrix);
    }
    unsafe fn MultMatrixx(&mut self, m: *const GLfixed) {
        self.multiply_matrix(&matrix_fixed_to_float(m));
    }
    unsafe fn PushMatrix(&mut self) {
        let stack = self.matrix_stack();
        let top = *stack.last().unwrap();
        stack.push(top);
    }
    unsafe fn PopMatrix(&mut self) {
        let stack = self.matrix_stack();
        // Popping the last matrix is an error (GL_STACK_UNDERFLOW) that
        // leaves the stack as it is.
        if stack.len() > 1 {
            stack.pop();
        }
    }
    unsafe fn Orthof(
        &mut self,
        left: GLfloat,
        right: GLfloat,
        bottom: GLfloat,
        top: GLfloat,
        near: GLfloat,
        far: GLfloat,
    ) {
        let (width, height, depth) = (right - left, top - bottom, far - near);
        #[rustfmt::skip]
        let matrix = [
            2.0 / width, 0.0, 0.0, 0.0,
            0.0, 2.0 / height, 0.0, 0.0,
            0.0, 0.0, -2.0 / depth, 0.0,
            -(right + left) / width, -(top + bottom) / height, -(far + near) / depth, 1.0,
        ];
        self.multiply_matrix(&matrix);
    }
    unsafe fn Orthox(
        &mut self,
        left: GLfixed,
        right: GLfixed,
        bottom: GLfixed,
        top: GLfixed,
        near: GLfixed,
        far: GLfixed,
    ) {
        self.Orthof(
            fixed_to_float(left),
            fixed_to_float(right),
            fixed_to_float(bottom),
            fixed_to_float(top),
            fixed_to_float(near),
            fixed_to_float(far),
        )
    }
    unsafe fn Frustumf(
        &mut self,
        left: GLfloat,
        right: GLfloat,
        bottom: GLfloat,
        top: GLfloat,
        near: GLfloat,
        far: GLfloat,
    ) {
        let (width, height, depth) = (right - left, top - bottom, far - near);
        #[rustfmt::skip]
        let matrix = [
            2.0 * near / width, 0.0, 0.0, 0.0,
            0.0, 2.0 * near / height, 0.0, 0.0,
            (right + left) / width, (top + bottom) / height, -(far + near) / depth, -1.0,
            0.0, 0.0, -2.0 * far * near / depth, 0.0,
        ];
        self.multiply_matrix(&matrix);
    }
    unsafe fn Frustumx(
        &mut self,
        left: GLfixed,
        right: GLfixed,
        bottom: GLfixed,
        top: GLfixed,
        near: GLfixed,
        far: GLfixed,
    ) {
        self.Frustumf(
            fixed_to_float(left),
            fixed_to_float(right),
            fixed_to_float(bottom),
            fixed_to_float(top),
            fixed_to_float(near),
            fixed_to_float(far),
        )
    }
    unsafe fn Rotatef(&mut self, angle: GLfloat, x: GLfloat, y: GLfloat, z: GLfloat) {
        let length = (x * x + y * y + z * z).sqrt();
        if length == 0.0 {
            return;
        }
        let (x, y, z) = (x / length, y / length, z / length);
        let (s, c) = angle.to_radians().sin_cos();
        let t = 1.0 - c;
        #[rustfmt::skip]
        let matrix = [
            x * x * t + c, y * x * t + z * s, x * z * t - y * s, 0.0,
            x * y * t - z * s, y * y * t + c, y * z * t + x * s, 0.0,
            x * z * t + y * s, y * z * t - x * s, z * z * t + c, 0.0,
            0.0, 0.0, 0.0, 1.0,
        ];
        self.multiply_matrix(&matrix);
    }
    unsafe fn Rotatex(&mut self, angle: GLfixed, x: GLfixed, y: GLfixed, z: GLfixed) {
        self.Rotatef(
            fixed_to_float(angle),
            fixed_to_float(x),
            fixed_to_float(y),
            fixed_to_float(z),
        )
    }
    unsafe fn Scalef(&mut self, x: GLfloat, y: GLfloat, z: GLfloat) {
        let mut matrix = IDENTITY;
        matrix[0] = x;
        matrix[5] = y;
        matrix[10] = z;
        self.multiply_matrix(&matrix);
    }
    unsafe fn Scalex(&mut self, x: GLfixed, y: GLfixed, z: GLfixed) {
        self.Scalef(fixed_to_float(x), fixed_to_float(y), fixed_to_float(z))
    }
    unsafe fn Translatef(&mut self, x: GLfloat, y: GLfloat, z: GLfloat) {
        let mut matrix = IDENTITY;
        matrix[12] = x;
        matrix[13] = y;
        matrix[14] = z;
        self.multiply_matrix(&matrix);
    }
    unsafe fn Translatex(&mut self, x: GLfixed, y: GLfixed, z: GLfixed) {
        self.Translatef(fixed_to_float(x), fixed_to_float(y), fixed_to_float(z))
    }

    // OES_framebuffer_object: this became part of OpenGL ES 2.0 itself, with
    // the same constants.
    unsafe fn GenFramebuffersOES(&mut self, n: GLsizei, framebuffers: *mut GLuint) {
        gl::GenFramebuffers(n, framebuffers);
    }
    unsafe fn GenRenderbuffersOES(&mut self, n: GLsizei, renderbuffers: *mut GLuint) {
        gl::GenRenderbuffers(n, renderbuffers);
    }
    unsafe fn IsFramebufferOES(&mut self, framebuffer: GLuint) -> GLboolean {
        gl::IsFramebuffer(framebuffer)
    }
    unsafe fn IsRenderbufferOES(&mut self, renderbuffer: GLuint) -> GLboolean {
        gl::IsRenderbuffer(renderbuffer)
    }
    unsafe fn BindFramebufferOES(&mut self, target: GLenum, framebuffer: GLuint) {
        gl::BindFramebuffer(target, framebuffer);
    }
    unsafe fn BindRenderbufferOES(&mut self, target: GLenum, renderbuffer: GLuint) {
        gl::BindRenderbuffer(target, renderbuffer);
    }
    unsafe fn RenderbufferStorageOES(
        &mut self,
        target: GLenum,
        internalformat: GLenum,
        width: GLsizei,
        height: GLsizei,
    ) {
        gl::RenderbufferStorage(target, internalformat, width, height);
    }
    unsafe fn FramebufferRenderbufferOES(
        &mut self,
        target: GLenum,
        attachment: GLenum,
        renderbuffertarget: GLenum,
        renderbuffer: GLuint,
    ) {
        gl::FramebufferRenderbuffer(target, attachment, renderbuffertarget, renderbuffer);
    }
    unsafe fn FramebufferTexture2DOES(
        &mut self,
        target: GLenum,
        attachment: GLenum,
        textarget: GLenum,
        texture: GLuint,
        level: i32,
    ) {
        gl::FramebufferTexture2D(target, attachment, textarget, texture, level);
    }
    unsafe fn GetFramebufferAttachmentParameterivOES(
        &mut self,
        target: GLenum,
        attachment: GLenum,
        pname: GLenum,
        params: *mut GLint,
    ) {
        gl::GetFramebufferAttachmentParameteriv(target, attachment, pname, params);
    }
    unsafe fn GetRenderbufferParameterivOES(
        &mut self,
        target: GLenum,
        pname: GLenum,
        params: *mut GLint,
    ) {
        gl::GetRenderbufferParameteriv(target, pname, params);
    }
    unsafe fn CheckFramebufferStatusOES(&mut self, target: GLenum) -> GLenum {
        gl::CheckFramebufferStatus(target)
    }
    unsafe fn DeleteFramebuffersOES(&mut self, n: GLsizei, framebuffers: *const GLuint) {
        gl::DeleteFramebuffers(n, framebuffers);
    }
    unsafe fn DeleteRenderbuffersOES(&mut self, n: GLsizei, renderbuffers: *const GLuint) {
        gl::DeleteRenderbuffers(n, renderbuffers);
    }
    unsafe fn GenerateMipmapOES(&mut self, target: GLenum) {
        gl::GenerateMipmap(target);
    }
    unsafe fn GetBufferParameteriv(&mut self, target: GLenum, pname: GLenum, params: *mut GLint) {
        gl::GetBufferParameteriv(target, pname, params);
    }
    unsafe fn MapBufferOES(&mut self, _target: GLenum, _access: GLenum) -> *mut GLvoid {
        // This is an extension in OpenGL ES 2.0, and WebGL can't have it.
        unimplemented!("GLES1-on-GLES2: glMapBufferOES")
    }
    unsafe fn UnmapBufferOES(&mut self, _target: GLenum) -> GLboolean {
        unimplemented!("GLES1-on-GLES2: glUnmapBufferOES")
    }
}

use std::ffi::{CStr, CString};
use std::num::NonZeroU32;
use std::ptr;
use glutin::config::{ConfigTemplateBuilder, GetGlConfig};
use glutin::context::{ContextAttributesBuilder, PossiblyCurrentContext};
use glutin::display::GetGlDisplay;
use glutin::prelude::*;
use glutin::surface::{Surface, SwapInterval, WindowSurface};
use glutin_winit::{DisplayBuilder, GlWindow};
use raw_window_handle::HasRawWindowHandle;
use winit::window::{Window, WindowBuilder};
const VERTEX_SHADER_SOURCE: &str = r#"
#version 330 core
layout (location = 0) in vec2 aPos;
layout (location = 1) in vec2 aTexCoord;
out vec2 TexCoord;
uniform vec4 uRect;  // x, y, width, height in normalized device coords
void main() {
    vec2 pos = uRect.xy + aPos * uRect.zw;
    gl_Position = vec4(pos, 0.0, 1.0);
    TexCoord = aTexCoord;
}
"#;
const FRAGMENT_SHADER_SOURCE: &str = r#"
#version 330 core
in vec2 TexCoord;
out vec4 FragColor;
uniform sampler2D uTexture;
void main() {
    FragColor = texture(uTexture, TexCoord);
}
"#;
const BORDER_FRAGMENT_SHADER: &str = r#"
#version 330 core
in vec2 TexCoord;
out vec4 FragColor;
uniform vec4 uColor;        // border color (RGBA)
uniform float uBorderWidth; // border width in UV space
uniform vec4 uCornerRadius; // corner radius for each corner
void main() {
    // Calculate distance from edge
    float left = TexCoord.x;
    float right = 1.0 - TexCoord.x;
    float top = TexCoord.y;
    float bottom = 1.0 - TexCoord.y;
    float dist = min(min(left, right), min(top, bottom));
    // Draw border if within border width
    if (dist < uBorderWidth) {
        FragColor = uColor;
    } else {
        discard;
    }
}
"#;
const SHADOW_FRAGMENT_SHADER: &str = r#"
#version 330 core
in vec2 TexCoord;
out vec4 FragColor;
uniform vec4 uShadowColor;  // shadow color with alpha
uniform float uSigma;       // blur amount
uniform vec2 uOffset;       // shadow offset in UV space
// Gaussian function
float gaussian(float x, float sigma) {
    return exp(-(x * x) / (2.0 * sigma * sigma));
}
void main() {
    // Distance from center
    vec2 center = vec2(0.5, 0.5);
    vec2 adjusted = TexCoord - uOffset;
    // Calculate fade based on distance from edges
    float fx = smoothstep(0.0, uSigma, adjusted.x) * smoothstep(0.0, uSigma, 1.0 - adjusted.x);
    float fy = smoothstep(0.0, uSigma, adjusted.y) * smoothstep(0.0, uSigma, 1.0 - adjusted.y);
    float fade = fx * fy;
    // Apply shadow with fade
    FragColor = uShadowColor * (1.0 - fade);
}
"#;
pub struct GlRenderer {
    pub window: Window,
    pub gl_context: PossiblyCurrentContext,
    pub gl_surface: Surface<WindowSurface>,
    pub width: u32,
     height: u32,
    shader_program: u32,
    border_shader_program: u32,
    shadow_shader_program: u32,
    vao: u32,
    vbo: u32,
}
impl GlRenderer {
    pub fn new(event_loop: &winit::event_loop::EventLoop<()>, title: &str, width: u32, height: u32) -> Result<Self, String> {
        let template = ConfigTemplateBuilder::new()
            .with_alpha_size(8)
            .with_transparency(false);
        let window_builder = WindowBuilder::new()
            .with_title(title)
            .with_transparent(false)
            .with_inner_size(winit::dpi::LogicalSize::new(width as f64, height as f64));
        let display_builder = DisplayBuilder::new().with_window_builder(Some(window_builder));
        let (window, gl_config) = display_builder
            .build(event_loop, template, |configs| {
                configs
                    .reduce(|accum, config| {
                        if config.num_samples() > accum.num_samples() {
                            config
                        } else {
                            accum
                        }
                    })
                    .unwrap()
            })
            .map_err(|e| format!("Failed to build display: {:?}", e))?;
        let window = window.ok_or("No window created")?;
        let raw_window_handle = window.raw_window_handle();
        let gl_display = gl_config.display();
        let context_attributes = ContextAttributesBuilder::new().build(Some(raw_window_handle));
        let not_current_context = unsafe {
            gl_display
                .create_context(&gl_config, &context_attributes)
                .map_err(|e| format!("Failed to create context: {:?}", e))?
        };
        let attrs = window.build_surface_attributes(Default::default());
        let gl_surface = unsafe {
            gl_display
                .create_window_surface(&gl_config, &attrs)
                .map_err(|e| format!("Failed to create window surface: {:?}", e))?
        };
        let gl_context = not_current_context
            .make_current(&gl_surface)
            .map_err(|e| format!("Failed to make current: {:?}", e))?;
        gl::load_with(|symbol| {
            let symbol = std::ffi::CString::new(symbol).expect("OpenGL symbol name contains null byte");
            gl_display.get_proc_address(symbol.as_c_str()).cast()
        });
        let version = unsafe { CStr::from_ptr(gl::GetString(gl::VERSION) as *const _).to_string_lossy() };
        log::info!("OpenGL Version: {}", version);
        if let Err(e) = gl_surface.set_swap_interval(&gl_context, SwapInterval::Wait(unsafe { NonZeroU32::new_unchecked(1) })) {
            log::warn!("Error setting vsync: {:?}", e);
        }
        let shader_program = unsafe { Self::compile_program(VERTEX_SHADER_SOURCE, FRAGMENT_SHADER_SOURCE)? };
        let border_shader_program = unsafe { Self::compile_program(VERTEX_SHADER_SOURCE, BORDER_FRAGMENT_SHADER)? };
        let shadow_shader_program = unsafe { Self::compile_program(VERTEX_SHADER_SOURCE, SHADOW_FRAGMENT_SHADER)? };
        let (vao, vbo) = unsafe { Self::create_quad_buffers() };
        let size = window.inner_size();
        Ok(Self {
            window,
            gl_context,
            gl_surface,
            width: size.width,
            height: size.height,
            shader_program,
            border_shader_program,
            shadow_shader_program,
            vao,
            vbo,
        })
    }
    unsafe fn compile_program(vertex_source: &str, fragment_source: &str) -> Result<u32, String> {
        let mut success = 0;
        let vertex_shader = gl::CreateShader(gl::VERTEX_SHADER);
        let c_str = CString::new(vertex_source).map_err(|e| format!("Failed to create CString: {}", e))?;
        gl::ShaderSource(vertex_shader, 1, &c_str.as_ptr(), ptr::null());
        gl::CompileShader(vertex_shader);
        gl::GetShaderiv(vertex_shader, gl::COMPILE_STATUS, &mut success);
        if success == 0 {
            let mut log = [0u8; 512];
            gl::GetShaderInfoLog(vertex_shader, 512, ptr::null_mut(), log.as_mut_ptr() as *mut _);
            return Err(format!("Vertex shader compilation failed: {}", String::from_utf8_lossy(&log)));
        }
        let fragment_shader = gl::CreateShader(gl::FRAGMENT_SHADER);
        let c_str = CString::new(fragment_source).map_err(|e| format!("Failed to create CString: {}", e))?;
        gl::ShaderSource(fragment_shader, 1, &c_str.as_ptr(), ptr::null());
        gl::CompileShader(fragment_shader);
        gl::GetShaderiv(fragment_shader, gl::COMPILE_STATUS, &mut success);
        if success == 0 {
            let mut log = [0u8; 512];
            gl::GetShaderInfoLog(fragment_shader, 512, ptr::null_mut(), log.as_mut_ptr() as *mut _);
            return Err(format!("Fragment shader compilation failed: {}", String::from_utf8_lossy(&log)));
        }
        let program = gl::CreateProgram();
        gl::AttachShader(program, vertex_shader);
        gl::AttachShader(program, fragment_shader);
        gl::LinkProgram(program);
        gl::GetProgramiv(program, gl::LINK_STATUS, &mut success);
        if success == 0 {
            let mut log = [0u8; 512];
            gl::GetProgramInfoLog(program, 512, ptr::null_mut(), log.as_mut_ptr() as *mut _);
            return Err(format!("Shader program linking failed: {}", String::from_utf8_lossy(&log)));
        }
        gl::DeleteShader(vertex_shader);
        gl::DeleteShader(fragment_shader);
        Ok(program)
    }
    unsafe fn create_quad_buffers() -> (u32, u32) {
        let vertices: [f32; 16] = [
            0.0, 0.0,   0.0, 1.0,   
            1.0, 0.0,   1.0, 1.0,   
            1.0, 1.0,   1.0, 0.0,   
            0.0, 1.0,   0.0, 0.0,   
        ];
        let mut vao = 0;
        let mut vbo = 0;
        gl::GenVertexArrays(1, &mut vao);
        gl::GenBuffers(1, &mut vbo);
        gl::BindVertexArray(vao);
        gl::BindBuffer(gl::ARRAY_BUFFER, vbo);
        gl::BufferData(
            gl::ARRAY_BUFFER,
            (vertices.len() * std::mem::size_of::<f32>()) as isize,
            vertices.as_ptr() as *const _,
            gl::STATIC_DRAW,
        );
        gl::VertexAttribPointer(0, 2, gl::FLOAT, gl::FALSE, 4 * std::mem::size_of::<f32>() as i32, ptr::null());
        gl::EnableVertexAttribArray(0);
        gl::VertexAttribPointer(1, 2, gl::FLOAT, gl::FALSE, 4 * std::mem::size_of::<f32>() as i32, (2 * std::mem::size_of::<f32>()) as *const _);
        gl::EnableVertexAttribArray(1);
        gl::BindBuffer(gl::ARRAY_BUFFER, 0);
        gl::BindVertexArray(0);
        (vao, vbo)
    }
    pub fn resize(&mut self, width: u32, height: u32) {
        if width > 0 && height > 0 {
            self.width = width;
            self.height = height;
            // Safety: We've checked that width > 0 and height > 0 above
            self.gl_surface.resize(
                &self.gl_context,
                unsafe { NonZeroU32::new_unchecked(width) },
                unsafe { NonZeroU32::new_unchecked(height) },
            );
            unsafe {
                gl::Viewport(0, 0, width as i32, height as i32);
            }
        }
    }
    pub fn clear(&self, r: f32, g: f32, b: f32, a: f32) {
        unsafe {
            gl::ClearColor(r, g, b, a);
            gl::Clear(gl::COLOR_BUFFER_BIT);
        }
    }
    pub fn draw_pixels(&self, x: i32, y: i32, dest_w: i32, dest_h: i32, tex_w: i32, tex_h: i32, pixels: &[u8]) {
        if dest_w <= 0 || dest_h <= 0 || tex_w <= 0 || tex_h <= 0 {
            return;
        }
        if pixels.len() < (tex_w * tex_h * 4) as usize {
            log::warn!("draw_pixels: buffer too small for texture");
            return;
        }
        unsafe {
            let mut texture: u32 = 0;
            gl::GenTextures(1, &mut texture);
            gl::BindTexture(gl::TEXTURE_2D, texture);
            gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_MIN_FILTER, gl::LINEAR as i32);
            gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_MAG_FILTER, gl::LINEAR as i32);
            gl::TexImage2D(
                gl::TEXTURE_2D,
                0,
                gl::RGBA as i32,
                tex_w,
                tex_h,
                0,
                gl::BGRA,
                gl::UNSIGNED_BYTE,
                pixels.as_ptr() as *const _,
            );
            let ndc_x = (2.0 * x as f32 / self.width as f32) - 1.0;
            let ndc_y = 1.0 - (2.0 * (y + dest_h) as f32 / self.height as f32);
            let ndc_w = 2.0 * dest_w as f32 / self.width as f32;
            let ndc_h = 2.0 * dest_h as f32 / self.height as f32;
            gl::UseProgram(self.shader_program);
            let rect_loc = gl::GetUniformLocation(self.shader_program, b"uRect\0".as_ptr() as *const _);
            gl::Uniform4f(rect_loc, ndc_x, ndc_y, ndc_w, ndc_h);
            gl::Enable(gl::BLEND);
            gl::BlendFunc(gl::SRC_ALPHA, gl::ONE_MINUS_SRC_ALPHA);
            gl::BindVertexArray(self.vao);
            gl::DrawArrays(gl::TRIANGLE_FAN, 0, 4);
            gl::BindVertexArray(0);
            gl::Disable(gl::BLEND);
            gl::DeleteTextures(1, &texture);
        }
    }
    pub fn draw_shadow(&self, x: i32, y: i32, width: i32, height: i32, sigma: f32) {
        if width <= 0 || height <= 0 { return; }
        unsafe {
            gl::UseProgram(self.shadow_shader_program);
            gl::Enable(gl::BLEND);
            gl::BlendFunc(gl::SRC_ALPHA, gl::ONE_MINUS_SRC_ALPHA);
            let rect_loc = gl::GetUniformLocation(self.shadow_shader_program, b"uRect\0".as_ptr() as *const _);
            let ndc_x = (2.0 * x as f32 / self.width as f32) - 1.0;
            let ndc_y = 1.0 - (2.0 * (y + height) as f32 / self.height as f32);
            let ndc_w = 2.0 * width as f32 / self.width as f32;
            let ndc_h = 2.0 * height as f32 / self.height as f32;
            gl::Uniform4f(rect_loc, ndc_x, ndc_y, ndc_w, ndc_h);
            let color_loc = gl::GetUniformLocation(self.shadow_shader_program, b"uShadowColor\0".as_ptr() as *const _);
            gl::Uniform4f(color_loc, 0.0, 0.0, 0.0, 0.5);  
            let sigma_loc = gl::GetUniformLocation(self.shadow_shader_program, b"uSigma\0".as_ptr() as *const _);
            gl::Uniform1f(sigma_loc, sigma);
            let offset_loc = gl::GetUniformLocation(self.shadow_shader_program, b"uOffset\0".as_ptr() as *const _);
            gl::Uniform2f(offset_loc, 0.0, 0.0);  
            gl::BindVertexArray(self.vao);
            gl::DrawArrays(gl::TRIANGLE_FAN, 0, 4);
            gl::BindVertexArray(0);
            gl::Disable(gl::BLEND);
        }
    }
    pub fn draw_border(&self, x: i32, y: i32, width: i32, height: i32, border_width: f32) {
        if width <= 0 || height <= 0 { return; }
        unsafe {
            gl::UseProgram(self.border_shader_program);
            gl::Enable(gl::BLEND);
            gl::BlendFunc(gl::SRC_ALPHA, gl::ONE_MINUS_SRC_ALPHA);
            let rect_loc = gl::GetUniformLocation(self.border_shader_program, b"uRect\0".as_ptr() as *const _);
            let ndc_x = (2.0 * x as f32 / self.width as f32) - 1.0;
            let ndc_y = 1.0 - (2.0 * (y + height) as f32 / self.height as f32);
            let ndc_w = 2.0 * width as f32 / self.width as f32;
            let ndc_h = 2.0 * height as f32 / self.height as f32;
            gl::Uniform4f(rect_loc, ndc_x, ndc_y, ndc_w, ndc_h);
            let color_loc = gl::GetUniformLocation(self.border_shader_program, b"uColor\0".as_ptr() as *const _);
            gl::Uniform4f(color_loc, 0.0, 0.6, 1.0, 1.0);  
            let width_loc = gl::GetUniformLocation(self.border_shader_program, b"uBorderWidth\0".as_ptr() as *const _);
            gl::Uniform1f(width_loc, border_width / width as f32);  
            gl::BindVertexArray(self.vao);
            gl::DrawArrays(gl::TRIANGLE_FAN, 0, 4);
            gl::BindVertexArray(0);
            gl::Disable(gl::BLEND);
        }
    }
    pub fn swap_buffers(&self) -> Result<(), String> {
        self.gl_surface
            .swap_buffers(&self.gl_context)
            .map_err(|e| format!("Failed to swap buffers: {:?}", e))
    }
    pub fn request_redraw(&self) {
        self.window.request_redraw();
    }
}
impl Drop for GlRenderer {
    fn drop(&mut self) {
        unsafe {
            gl::DeleteProgram(self.shader_program);
            gl::DeleteProgram(self.border_shader_program);
            gl::DeleteProgram(self.shadow_shader_program);
            gl::DeleteVertexArrays(1, &self.vao);
            gl::DeleteBuffers(1, &self.vbo);
        }
    }
}
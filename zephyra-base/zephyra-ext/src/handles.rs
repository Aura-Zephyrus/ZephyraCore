//! RAII 句柄（§3.5，P0-2/S7）。

use std::borrow::Cow;
use std::cell::RefCell;
use std::rc::Weak;

use zephyra_render_ext::prelude::{FrameDesc, RegisteredTexture};

use crate::debug;
use crate::ext_context::{ExtContext, ExtError};

/// 外部纹理 RAII 句柄：Drop 时自动 unregister。仅 UI/渲染线程。
pub struct ExternalTextureHandle {
    key: Cow<'static, str>,
    ctx: Weak<RefCell<ExtContext>>,
    unregister_on_drop: bool,
}

impl ExternalTextureHandle {
    pub(crate) fn new(
        key: Cow<'static, str>,
        ctx: Weak<RefCell<ExtContext>>,
        unregister_on_drop: bool,
    ) -> Self {
        Self {
            key,
            ctx,
            unregister_on_drop,
        }
    }
    pub fn key(&self) -> &str {
        &self.key
    }
    /// 同 key 再提交一帧（S7 原地替换）。
    ///
    /// 与扩展上下文的 submit_frame 走同一个动作，不存在独立的
    /// 「更新」路径。`valid_until` 必须大于当前帧号。
    pub fn update(
        &self,
        desc: FrameDesc,
        valid_until: zephyra_render_ext::prelude::FrameId,
    ) -> Result<RegisteredTexture, ExtError> {
        let ctx = self.ctx.upgrade().ok_or(ExtError::ContextDropped)?;
        {
            let c = ctx.borrow();
            if valid_until <= c.frame_id() {
                return Err(ExtError::ExpiredFrame {
                    valid_until,
                    current: c.frame_id(),
                });
            }
        }
        let result = ctx
            .borrow_mut()
            .submit_frame(self.key.clone(), desc, valid_until);
        result
    }
    pub fn try_unregister(&self) -> Result<(), ExtError> {
        let Some(ctx) = self.ctx.upgrade() else {
            return Err(ExtError::ContextDropped);
        };
        let r = ctx.borrow_mut().unregister_texture(&self.key);
        r
    }
    pub fn unregister(self) -> Result<(), ExtError> {
        self.try_unregister()
    }
}

impl Drop for ExternalTextureHandle {
    fn drop(&mut self) {
        if !self.unregister_on_drop {
            return;
        }
        if let Some(ctx) = self.ctx.upgrade() {
            if let Err(e) = ctx.borrow_mut().unregister_texture(&self.key) {
                debug::log_texture(&format!("drop unregister failed key={}: {e}", self.key));
            }
        }
    }
}

/// Hook RAII 句柄。`window = None` 为全局，`Some(id)` 为窗口作用域。
pub struct HookHandle {
    kind: Cow<'static, str>,
    window: Option<u64>,
    ctx: Weak<RefCell<ExtContext>>,
}

impl HookHandle {
    pub(crate) fn new(
        kind: Cow<'static, str>,
        window: Option<u64>,
        ctx: Weak<RefCell<ExtContext>>,
    ) -> Self {
        Self { kind, window, ctx }
    }
    pub fn kind(&self) -> &str {
        &self.kind
    }
    pub fn window(&self) -> Option<u64> {
        self.window
    }
}

impl Drop for HookHandle {
    fn drop(&mut self) {
        if let Some(ctx) = self.ctx.upgrade() {
            let _ = ctx.borrow_mut().unregister_hook(&self.kind, self.window);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ext_context::{ExtContextHandle, ExtContextImpl};
    use std::cell::Cell;
    use std::rc::Rc;
    use zephyra_render_ext::prelude::{BackendCapabilities, DrawHook, FrameId, RegisteredTexture};

    #[test]
    fn external_texture_handle_key_and_dead_context_update() {
        let h = ExternalTextureHandle::new(Cow::Borrowed("tex-key"), Weak::new(), true);
        assert_eq!(h.key(), "tex-key");
        // A dead Weak cannot be upgraded -> ContextDropped before any frame check.
        let err = h
            .update(FrameDesc::rgba(vec![0; 4], 1, 1, 4), FrameId(2))
            .unwrap_err();
        assert!(matches!(err, ExtError::ContextDropped));
    }

    #[test]
    fn hook_handle_kind_and_window_getters() {
        let global = HookHandle::new(Cow::Borrowed("debug"), None, Weak::new());
        assert_eq!(global.kind(), "debug");
        assert_eq!(global.window(), None);
        let scoped = HookHandle::new(Cow::Borrowed("per-win"), Some(42), Weak::new());
        assert_eq!(scoped.kind(), "per-win");
        assert_eq!(scoped.window(), Some(42));
    }

    /// Smallest live context needed to exercise `ExternalTextureHandle::update`.
    struct MockImpl {
        frame: FrameId,
        frames: Rc<Cell<u32>>,
    }

    impl ExtContextImpl for MockImpl {
        fn submit_frame(
            &mut self,
            key: Cow<'static, str>,
            _desc: FrameDesc,
            valid_until: FrameId,
        ) -> Result<RegisteredTexture, ExtError> {
            self.frames.set(self.frames.get() + 1);
            Ok(RegisteredTexture { key, valid_until })
        }
        fn unregister_texture(&mut self, _key: &str) -> Result<(), ExtError> {
            Ok(())
        }
        fn register_hook(
            &mut self,
            _kind: Cow<'static, str>,
            _hook: Box<DrawHook>,
        ) -> Result<(), ExtError> {
            Ok(())
        }
        fn unregister_hook(&mut self, _kind: &str, _window: Option<u64>) -> bool {
            true
        }
        fn capabilities(&self) -> BackendCapabilities {
            BackendCapabilities::none()
        }
        fn frame_id(&self) -> FrameId {
            self.frame
        }
    }

    #[test]
    fn external_texture_handle_update_valid_expired_and_context_dropped() {
        let frames = Rc::new(Cell::new(0));
        let h = ExtContextHandle::new(Box::new(MockImpl {
            frame: FrameId(1),
            frames: frames.clone(),
        }));
        let tex = h
            .submit_frame("u", FrameDesc::rgba(vec![0; 4], 1, 1, 4), FrameId(10))
            .expect("submit");
        assert_eq!(tex.key(), "u");
        let after_submit = frames.get();

        // valid_until <= current frame is rejected before reaching the backend.
        let err = tex
            .update(FrameDesc::rgba(vec![1; 4], 1, 1, 4), FrameId(1))
            .unwrap_err();
        assert!(
            matches!(err, ExtError::ExpiredFrame { valid_until, current } if valid_until == FrameId(1) && current == FrameId(1)),
            "unexpected error: {err:?}"
        );
        assert_eq!(
            frames.get(),
            after_submit,
            "expired update must not reach the backend"
        );

        // A valid update goes through the same submit_frame path.
        tex.update(FrameDesc::rgba(vec![2; 4], 1, 1, 4), FrameId(11))
            .expect("valid update");
        assert_eq!(frames.get(), after_submit + 1);

        // Dropping the context handle invalidates the Weak inside the texture handle.
        drop(h);
        let err = tex
            .update(FrameDesc::rgba(vec![3; 4], 1, 1, 4), FrameId(12))
            .unwrap_err();
        assert!(matches!(err, ExtError::ContextDropped));
    }
}

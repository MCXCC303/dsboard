//! DSBoard - AstroBox v2 插件入口(wasm32-wasip2 Component)。
//!
//! 注意HTTP动作(查余额/导出)是阻塞式的,执行期间该插件的事件分发会暂停。

wit_bindgen::generate!({
    path: "wit",
    world: "psys-world",
    generate_all,
});

use exports::astrobox::psys_plugin::{event, lifecycle};
use wit_bindgen::FutureReader;

mod backup;
mod dates;
mod deepseek;
mod engine;
mod import;
mod logger;
mod snapshot;
mod state;
mod ui;

struct Plugin;

/// 返回一个"已完成"的 FutureReader<String>
fn immediate_string(value: String) -> FutureReader<String> {
    let (writer, reader) = wit_future::new(String::new);
    wit_bindgen::spawn(async move {
        let _ = writer.write(value).await;
    });
    reader
}

/// 返回一个"已完成"的 FutureReader<()>
fn immediate_unit() -> FutureReader<()> {
    let (writer, reader) = wit_future::new::<()>(|| ());
    wit_bindgen::spawn(async move {
        let _ = writer.write(()).await;
    });
    reader
}

impl lifecycle::Guest for Plugin {
    fn on_load() {
        logger::init();
        wit_bindgen::block_on(async {
            engine::init().await;
        });
        tracing::info!("DSBoard 插件加载完成");
    }
}

impl event::Guest for Plugin {
    fn on_event(event_type: event::EventType, event_payload: String) -> FutureReader<String> {
        // 同步入口日志:确认宿主是否调用了本回调
        tracing::info!("[entry] on_event type={event_type:?} payload={event_payload}");

        wit_bindgen::block_on(engine::handle_event(event_type, &event_payload));

        immediate_string(String::new())
    }

    fn on_ui_event(
        event_id: String,
        event: event::Event,
        event_payload: String,
    ) -> FutureReader<String> {
        tracing::info!("[entry] on_ui_event id={event_id} ev={event:?} payload={event_payload}");

        // 同步处理:block_on 驱动其中的宿主调用,完成后立即重绘
        wit_bindgen::block_on(ui::handle_ui_event(&event_id, &event, &event_payload));

        immediate_string(String::new())
    }

    fn on_ui_render(element_id: String) -> FutureReader<()> {
        tracing::info!("[entry] on_ui_render id={element_id}");

        // 宿主渲染是同步 ABI:先完成渲染,再返回已解决的 future
        ui::render_page(&element_id);

        immediate_unit()
    }

    fn on_card_render(card_id: String) -> FutureReader<()> {
        tracing::info!("[entry] on_card_render id={card_id}");

        ui::render_card(&card_id);

        immediate_unit()
    }
}

export!(Plugin);

//! Native analysis workspace. All network work uses the shared service runtime.
mod overlay;
use super::App;
use earthcraft_core::language::Lang;
use earthcraft_data::analysis::AnalysisClient;
use eframe::egui::{self, TextureHandle};
use serde_json::{Value, json};
use std::{
    sync::mpsc,
    time::{Duration, Instant},
};

const TOOL_IDS: [&str; 5] = [
    "distance",
    "area",
    "ndvi_summary",
    "ndvi_change",
    "elevation_summary",
];
const MAX_POINTS: usize = 128;
const POLL_INTERVAL: Duration = Duration::from_secs(1);

type Command = (Action, &'static str, String, Option<Value>);

enum Action {
    Models,
    Verify,
    Submit,
    Poll,
    Cancel,
    Tiles,
}
enum Reply {
    Json(Value),
    Tiles(Vec<(overlay::Tile, image::RgbaImage)>),
}

pub(super) struct Analysis {
    pub open: bool,
    pub points: Vec<[f64; 2]>,
    polygon: bool,
    provider: String,
    models: Vec<Value>,
    model: String,
    question: String,
    tool: usize,
    dates: [String; 4],
    client: Option<AnalysisClient>,
    pending: Option<(Action, mpsc::Receiver<Result<Reply, String>>)>,
    job: Option<String>,
    poll_after: Instant,
    status: String,
    result: Option<Value>,
    result_id: Option<String>,
    use_result: bool,
    loaded_models: bool,
    tiles: Vec<(overlay::Tile, TextureHandle)>,
    cancel_requested: bool,
    pub visible: bool,
}
impl Default for Analysis {
    fn default() -> Self {
        Self {
            open: false,
            points: Vec::new(),
            polygon: true,
            provider: "ollama".into(),
            models: Vec::new(),
            model: String::new(),
            question: String::new(),
            tool: 1,
            dates: Default::default(),
            client: None,
            pending: None,
            job: None,
            poll_after: Instant::now(),
            status: String::new(),
            result: None,
            result_id: None,
            use_result: false,
            loaded_models: false,
            tiles: Vec::new(),
            cancel_requested: false,
            visible: true,
        }
    }
}
impl Analysis {
    pub(super) fn add_point(&mut self, lat: f64, lon: f64) {
        if self.points.len() < MAX_POINTS {
            self.points.push([lon, lat]);
        } else {
            self.status = "Selection point limit reached.".into();
        }
    }
    fn geometry(&self) -> Option<Value> {
        if self.polygon && self.points.len() >= 3 {
            let mut ring = self.points.clone();
            ring.push(ring[0]);
            Some(json!({"type":"Polygon","coordinates":[ring]}))
        } else if !self.polygon && self.points.len() >= 2 {
            Some(json!({"type":"LineString","coordinates":self.points}))
        } else {
            None
        }
    }
    fn context(&self) -> Value {
        let mut context = json!({});
        if let Some(geometry) = self.geometry() {
            context["geometry"] = geometry;
        }
        for (key, date) in ["start", "end", "compare_start", "compare_end"]
            .iter()
            .zip(&self.dates)
        {
            if !date.trim().is_empty() {
                context[*key] = json!(date.trim());
            }
        }
        context
    }
    fn selection_controls(&mut self, ui: &mut egui::Ui, lang: Lang, center: [f64; 2]) {
        ui.horizontal(|ui| {
            if ui
                .selectable_value(&mut self.polygon, true, lang.text("pulse-region"))
                .changed()
                && self.tool == 0
            {
                self.tool = 1;
            }
            if ui
                .selectable_value(&mut self.polygon, false, lang.text("pulse-line"))
                .changed()
            {
                self.tool = 0;
            }
        });
        ui.small(lang.text("pulse-point-hint"));
        ui.horizontal(|ui| {
            if ui.button(lang.text("pulse-add-center")).clicked() {
                self.add_point(center[1], center[0]);
            }
            if ui.button(lang.text("pulse-undo")).clicked() {
                self.points.pop();
            }
            if ui.button(lang.text("pulse-clear")).clicked() {
                self.points.clear();
            }
            ui.monospace(self.points.len().to_string());
        });
        egui::Grid::new("pulse-dates").show(ui, |ui| {
            let labels = [
                "pulse-start",
                "pulse-end",
                "pulse-compare-start",
                "pulse-compare-end",
            ];
            for (date, label) in self.dates.iter_mut().zip(labels) {
                ui.label(lang.text(label));
                ui.add(
                    egui::TextEdit::singleline(date)
                        .desired_width(110.)
                        .hint_text("YYYY-MM-DD"),
                );
                ui.end_row();
            }
        });
    }

    fn model_controls(&mut self, ui: &mut egui::Ui, lang: Lang) -> Option<Command> {
        let mut action = None;
        ui.horizontal(|ui| {
            ui.label(lang.text("pulse-provider"));
            let previous = self.provider.clone();
            egui::ComboBox::from_id_salt("pulse-provider")
                .selected_text(&self.provider)
                .show_ui(ui, |ui| {
                    for provider in ["ollama", "nvidia"] {
                        ui.selectable_value(&mut self.provider, provider.into(), provider);
                    }
                });
            let refresh = ui.button(lang.text("pulse-refresh")).clicked();
            if self.provider != previous || refresh {
                self.models.clear();
                self.model.clear();
                action = Some((
                    Action::Models,
                    "GET",
                    format!("/models?provider={}", self.provider),
                    None,
                ));
            }
        });
        ui.horizontal(|ui| {
            ui.label(lang.text("pulse-model"));
            let label = if self.model.is_empty() {
                "—"
            } else {
                self.model.as_str()
            };
            egui::ComboBox::from_id_salt("pulse-model")
                .selected_text(label)
                .show_ui(ui, |ui| {
                    for row in &self.models {
                        if let Some(id) = row["id"].as_str() {
                            ui.selectable_value(&mut self.model, id.into(), id);
                        }
                    }
                });
            let verify = egui::Button::new(lang.text("pulse-verify"));
            if ui.add_enabled(!self.model.is_empty(), verify).clicked() {
                self.status = lang.text("pulse-verifying").into();
                let body = json!({"provider": self.provider, "model": self.model});
                action = Some((Action::Verify, "POST", "/verify".into(), Some(body)));
            }
        });
        if let Some(row) = self
            .models
            .iter()
            .find(|row| row["id"].as_str() == Some(self.model.as_str()))
        {
            let checks = [
                ("text", "pulse-text"),
                ("tools", "pulse-tools"),
                ("vision", "pulse-vision"),
            ];
            let verified = checks
                .iter()
                .filter_map(|(key, label)| {
                    (row["verified"][*key] == true).then_some(lang.text(label))
                })
                .collect::<Vec<_>>();
            let summary = if verified.is_empty() {
                lang.text("pulse-unverified").to_owned()
            } else {
                verified.join(", ")
            };
            ui.label(format!("{} {summary}", lang.text("pulse-verified")));
            if let Some(notes) = row["notes"].as_object().filter(|notes| !notes.is_empty()) {
                ui.collapsing(lang.text("pulse-evidence"), |ui| {
                    for (name, note) in notes {
                        if let Some(message) = note.as_str() {
                            ui.label(format!("{name} · {message}"));
                        }
                    }
                });
            }
        }
        action
    }
    fn controls(&mut self, ui: &mut egui::Ui, lang: Lang, center: [f64; 2]) -> Option<Command> {
        let mut action = None;
        let idle = self.pending.is_none() && self.job.is_none();
        ui.add_enabled_ui(idle, |ui| {
            action = self.model_controls(ui, lang);
            ui.separator();
            self.selection_controls(ui, lang, center);
            ui.separator();
            ui.label(lang.text("pulse-question"));
            ui.text_edit_multiline(&mut self.question);
            if self.result_id.is_some() {
                ui.checkbox(&mut self.use_result, lang.text("pulse-use-result"));
            }
            let can_chat = self.models.iter().any(|row| {
                row["id"].as_str() == Some(self.model.as_str()) && row["verified"]["text"] == true
            });
            let ask = egui::Button::new(lang.text("pulse-ask"));
            if ui
                .add_enabled(can_chat && !self.question.trim().is_empty(), ask)
                .clicked()
            {
                let mut body = json!({
                    "question": self.question.trim(),
                    "provider": self.provider,
                    "model": self.model,
                    "context": self.context()
                });
                if self.use_result
                    && let Some(id) = &self.result_id
                {
                    body["previous_result_id"] = json!(id);
                }
                action = Some((Action::Submit, "POST", "/chat".into(), Some(body)));
            }
            ui.horizontal(|ui| {
                egui::ComboBox::from_id_salt("pulse-tool")
                    .selected_text(tool_label(lang, self.tool))
                    .show_ui(ui, |ui| {
                        for i in 0..TOOL_IDS.len() {
                            ui.selectable_value(&mut self.tool, i, tool_label(lang, i));
                        }
                    });
                let can_run = self.geometry().is_some() && (self.tool == 0) != self.polygon;
                let run = egui::Button::new(lang.text("pulse-run"));
                if ui.add_enabled(can_run, run).clicked() {
                    let body = json!({ "tool": TOOL_IDS[self.tool], "context": self.context() });
                    action = Some((Action::Submit, "POST", "/analyses".into(), Some(body)));
                }
            });
        });
        if self.job.is_some() && ui.button(lang.text("pulse-cancel")).clicked() {
            self.cancel_requested = true;
        }
        if !idle {
            ui.spinner();
        }
        if !self.status.is_empty() {
            ui.label(&self.status);
        }
        if let Some(r) = &self.result {
            ui.separator();
            ui.heading(lang.text("pulse-result"));
            ui.checkbox(&mut self.visible, lang.text("pulse-show-layer"));
            if let Some(explanation) = r["explanation"].as_str() {
                ui.label(explanation);
            }
            if let Some(stats) = r["statistics"].as_object() {
                ui.strong(lang.text("pulse-statistics"));
                for (key, value) in stats {
                    ui.label(format!("{} · {value}", key.replace('_', " ")));
                }
            }
            if !r["provenance"].is_null() {
                ui.collapsing(lang.text("pulse-provenance"), |ui| {
                    ui.monospace(format!("{:#}", r["provenance"]));
                });
            }
            if let Some(warnings) = r["warnings"].as_array() {
                for warning in warnings.iter().filter_map(Value::as_str) {
                    ui.label(warning);
                }
            }
        }
        action
    }
}
fn tool_label(lang: Lang, index: usize) -> String {
    lang.text(
        [
            "pulse-distance",
            "pulse-area",
            "pulse-ndvi",
            "pulse-change",
            "pulse-elevation",
        ][index],
    )
    .into()
}

impl App {
    fn analysis_request(
        &mut self,
        action: Action,
        method: &'static str,
        path: String,
        body: Option<Value>,
        ctx: &egui::Context,
    ) {
        let client = match self
            .analysis
            .client
            .clone()
            .map(Ok)
            .unwrap_or_else(AnalysisClient::connect)
        {
            Ok(client) => client,
            Err(e) => {
                self.analysis.status = e;
                return;
            }
        };
        self.analysis.client = Some(client.clone());
        let (tx, rx) = mpsc::channel();
        let wake = ctx.clone();
        self.analysis.pending = Some((action, rx));
        self.runtime.spawn(async move {
            let _ = tx.send(client.json(method, &path, body).await.map(Reply::Json));
            wake.request_repaint();
        });
    }
    pub(super) fn analysis_window(&mut self, ctx: &egui::Context) {
        self.poll_analysis(ctx);
        if !self.analysis.open {
            return;
        }
        if !self.analysis.loaded_models && self.analysis.pending.is_none() {
            self.analysis.loaded_models = true;
            self.analysis_request(
                Action::Models,
                "GET",
                format!("/models?provider={}", self.analysis.provider),
                None,
                ctx,
            );
        }
        let mut open = true;
        let mut command = None;
        let lg = self.lang;
        egui::Window::new(lg.text("pulse-title"))
            .open(&mut open)
            .default_width(390.0)
            .vscroll(true)
            .show(ctx, |ui| {
                command = self.analysis.controls(ui, lg, [self.cam.lon, self.cam.lat]);
            });
        self.analysis.open = open;
        if let Some((action, method, path, body)) = command {
            self.analysis_request(action, method, path, body, ctx);
        }
    }
    fn poll_analysis(&mut self, ctx: &egui::Context) {
        let mut layer = None;
        if let Some((action, rx)) = self.analysis.pending.take() {
            match rx.try_recv() {
                Err(mpsc::TryRecvError::Empty) => {
                    self.analysis.pending = Some((action, rx));
                    return;
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.analysis.status = "Backend worker disconnected.".into();
                    self.analysis.job = None;
                }
                Ok(Err(e)) => {
                    self.analysis.status = e;
                    self.analysis.job = None;
                    self.analysis.cancel_requested = false;
                }
                Ok(Ok(Reply::Tiles(tiles))) => {
                    self.analysis.tiles = tiles
                        .into_iter()
                        .map(|(t, img)| {
                            let image = egui::ColorImage::from_rgba_unmultiplied(
                                [img.width() as usize, img.height() as usize],
                                img.as_raw(),
                            );
                            (
                                t,
                                ctx.load_texture(
                                    "analysis-raster",
                                    image,
                                    egui::TextureOptions::LINEAR,
                                ),
                            )
                        })
                        .collect();
                }
                Ok(Ok(Reply::Json(v))) => match action {
                    Action::Models => {
                        self.analysis.models = v["models"].as_array().cloned().unwrap_or_default();
                        self.analysis.model = self
                            .analysis
                            .models
                            .first()
                            .and_then(|m| m["id"].as_str())
                            .unwrap_or("")
                            .into();
                        self.analysis.status = v["error"].as_str().unwrap_or("").to_owned();
                    }
                    Action::Verify => {
                        if let Some(row) =
                            self.analysis.models.iter_mut().find(|r| r["id"] == v["id"])
                        {
                            *row = v;
                        }
                        self.analysis.status = String::new();
                    }
                    Action::Submit => {
                        self.analysis.job = v["id"].as_str().map(str::to_owned);
                        self.analysis.poll_after = Instant::now();
                        self.analysis.result = None;
                        self.analysis.result_id = None;
                        self.analysis.use_result = false;
                        self.analysis.tiles.clear();
                        self.analysis.cancel_requested = false;
                        self.analysis.visible = true;
                        self.analysis.status = if self.analysis.job.is_some() {
                            "queued"
                        } else {
                            "Missing analysis job ID."
                        }
                        .into();
                    }
                    Action::Poll | Action::Cancel => {
                        let status = v["status"].as_str().unwrap_or("error");
                        self.analysis.status = v["error"].as_str().unwrap_or(status).into();
                        if status == "done" {
                            self.analysis.result_id = self
                                .analysis
                                .job
                                .take()
                                .filter(|_| !v["result"]["statistics"].is_null());
                            if let Some(path) = v["result"]["layer"]["tiles"].as_str() {
                                layer = Some((
                                    path.to_owned(),
                                    overlay::positions(&v["result"]["geometry"]),
                                ));
                            }
                            self.analysis.result = Some(v["result"].clone());
                        } else if matches!(status, "queued" | "running") {
                            self.analysis.poll_after = Instant::now() + POLL_INTERVAL;
                        } else {
                            self.analysis.job = None;
                        }
                    }
                    Action::Tiles => {}
                },
            }
        }
        if let Some((path, points)) = layer
            && let Some(client) = self.analysis.client.clone()
        {
            let (tx, rx) = mpsc::channel();
            let wake = ctx.clone();
            self.analysis.pending = Some((Action::Tiles, rx));
            self.runtime.spawn(async move {
                let _ = tx.send(
                    overlay::load(&client, &path, &points)
                        .await
                        .map(Reply::Tiles),
                );
                wake.request_repaint();
            });
        }
        if let Some(job) = self.analysis.job.clone() {
            let cancel = std::mem::take(&mut self.analysis.cancel_requested);
            if cancel || self.analysis.poll_after <= Instant::now() {
                self.analysis_request(
                    if cancel { Action::Cancel } else { Action::Poll },
                    if cancel { "DELETE" } else { "GET" },
                    format!("/jobs/{job}"),
                    None,
                    ctx,
                );
            } else {
                ctx.request_repaint_after(POLL_INTERVAL);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globe_selection_preserves_lon_lat_and_does_not_close_a_line() {
        let mut analysis = Analysis::default();
        analysis.add_point(19., 72.);
        analysis.add_point(18., 73.);
        analysis.add_point(18., 72.);
        assert_eq!(
            analysis.context()["geometry"]["coordinates"],
            json!([[[72., 19.], [73., 18.], [72., 18.], [72., 19.]]])
        );
        analysis.polygon = false;
        assert_eq!(
            analysis.context()["geometry"]["coordinates"],
            json!([[72., 19.], [73., 18.], [72., 18.]])
        );
    }
}

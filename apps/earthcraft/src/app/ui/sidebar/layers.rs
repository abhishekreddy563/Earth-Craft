use crate::app::App;
use earthcraft_data::tiles::Provider;
use eframe::egui::{self, Color32};

impl App {
    pub(super) fn sidebar_layers(&mut self, ui: &mut egui::Ui) {
        egui::CollapsingHeader::new(self.lang.text("sidebar-imagery-title"))
            .id_salt("layers-imagery")
            .default_open(true)
            .show(ui, |ui| self.layer_imagery(ui));
        egui::CollapsingHeader::new(self.lang.text("sidebar-geography-title"))
            .id_salt("layers-geography")
            .default_open(true)
            .show(ui, |ui| self.layer_geography(ui));
        egui::CollapsingHeader::new(self.lang.text("sidebar-sky-title"))
            .id_salt("layers-sky")
            .default_open(true)
            .show(ui, |ui| self.layer_sky(ui));
        egui::CollapsingHeader::new(self.lang.text("sidebar-weather-title"))
            .id_salt("layers-weather")
            .default_open(true)
            .show(ui, |ui| self.layer_weather(ui));
        egui::CollapsingHeader::new(self.lang.text("sidebar-live-objects-title"))
            .id_salt("layers-objects")
            .default_open(true)
            .show(ui, |ui| self.layer_objects(ui));
        egui::CollapsingHeader::new(self.lang.text("sidebar-transport-title"))
            .id_salt("layers-transport")
            .default_open(true)
            .show(ui, |ui| self.layer_transport(ui));
        ui.small(self.lang.text("layers-live-overlays-help"));
    }
    fn layer_imagery(&mut self, ui: &mut egui::Ui) {
        let lg = self.lang;
        for p in Provider::ALL {
            ui.radio_value(&mut self.provider, p, p.name(lg));
        }
        if self.provider == Provider::Wayback {
            ui.indent("wayback_ui", |ui| {
                if self.wayback_list.is_empty() {
                    ui.weak(lg.text("wayback-loading-status"));
                } else {
                    let n = self.wayback_list.len();
                    let mut idx = self.wayback_idx;
                    ui.horizontal(|ui| {
                        if ui
                            .small_button("◀")
                            .on_hover_text(lg.text("wayback-older-tooltip"))
                            .clicked()
                            && idx + 1 < n
                        {
                            idx += 1;
                        }
                        egui::ComboBox::from_id_salt("wayback_date")
                            .selected_text(self.wayback_list[idx].date.clone())
                            .height(320.0)
                            .show_ui(ui, |ui| {
                                super::super::selection::size_menu(
                                    ui,
                                    self.wayback_list
                                        .iter()
                                        .map(|release| release.date.as_str()),
                                );
                                for (i, r) in self.wayback_list.iter().enumerate() {
                                    super::super::selection::full_width_value(
                                        ui, &mut idx, i, &r.date,
                                    );
                                }
                            });
                        if ui
                            .small_button("▶")
                            .on_hover_text(lg.text("wayback-newer-tooltip"))
                            .clicked()
                            && idx > 0
                        {
                            idx -= 1;
                        }
                    });
                    ui.weak(lg.format(
                        "wayback-availability-status",
                        &[("count", n.to_string().into())],
                    ));
                    ui.weak(lg.text("wayback-fallback-help"));
                    self.set_wayback(idx);
                }
                if let Some(e) = &self.wayback_err {
                    ui.colored_label(Color32::from_rgb(255, 120, 120), format!("✖ {e}"));
                }
            });
        }
    }
    fn layer_geography(&mut self, ui: &mut egui::Ui) {
        let lg = self.lang;
        ui.checkbox(
            &mut self.show_country_boundaries,
            lg.text("layers-country-borders-toggle"),
        );
        ui.checkbox(
            &mut self.show_state_boundaries,
            lg.text("layers-state-borders-toggle"),
        );
        ui.checkbox(
            &mut self.show_place_labels,
            lg.text("layers-place-labels-toggle"),
        );
        ui.checkbox(
            &mut self.show_city_boundaries,
            lg.text("layers-municipal-borders-toggle"),
        );
        ui.checkbox(
            &mut self.show_metro_boundaries,
            lg.text("layers-metro-borders-toggle"),
        );
        if self.boundary_loader.is_loading_base_layers() {
            ui.weak(lg.text("boundaries-loading-status"));
        }
        if self.boundary_loader.is_loading_world_cities() {
            ui.weak(lg.text("boundaries-osm-loading-status"));
        }
        if let Ok(log) = self.boundary_log.lock() {
            for line in log.iter() {
                ui.weak(egui::RichText::new(line).small());
            }
        }
        ui.checkbox(&mut self.show_terrain, lg.text("layers-terrain-toggle"));
        if self.show_terrain {
            ui.indent("terrain_controls", |ui| {
                ui.add(
                    egui::Slider::new(&mut self.terrain_exaggeration, 0.5..=3.0)
                        .text(lg.text("layers-terrain-exaggeration")),
                );
                ui.weak(lg.text("layers-terrain-help"));
                ui.hyperlink_to("© Mapterhorn", "https://mapterhorn.com/attribution");
            });
        }
        ui.checkbox(&mut self.show_grid, lg.text("layers-grid-toggle"));
        if self.show_grid {
            ui.indent("grid_numbers", |ui| {
                ui.checkbox(
                    &mut self.settings.grid_numbers,
                    lg.text("layers-grid-numbers-toggle"),
                );
            });
        }
    }
    fn layer_transport(&mut self, ui: &mut egui::Ui) {
        let lg = self.lang;
        ui.checkbox(&mut self.show_roads, lg.text("layers-roads-toggle"));
        if self.show_roads {
            ui.small(
                self.roads
                    .service
                    .status(self.cam.alt, lg, self.distance_units()),
            );
            ui.hyperlink_to(
                "© OpenStreetMap contributors (ODbL)",
                "https://www.openstreetmap.org/copyright",
            );
        }
        ui.checkbox(&mut self.show_traffic, lg.text("layers-traffic-toggle"));
        if self.show_traffic {
            ui.add(
                egui::TextEdit::singleline(&mut self.tomtom_key)
                    .password(true)
                    .hint_text(lg.text("layers-traffic-key")),
            );
            ui.small(
                self.traffic
                    .service
                    .status(&self.tomtom_key, self.cam.alt, lg),
            );
            ui.hyperlink_to("TomTom Traffic", "https://developer.tomtom.com/");
        }
    }
    fn layer_weather(&mut self, ui: &mut egui::Ui) {
        let lg = self.lang;
        ui.checkbox(&mut self.show_weather, lg.text("layers-weather-toggle"));
        if self.show_weather {
            ui.hyperlink_to(
                lg.text("sidebar-radar-source"),
                "https://www.rainviewer.com/",
            );
        }
        ui.checkbox(&mut self.show_clouds, lg.text("layers-clouds-toggle"));
        if self.show_clouds {
            ui.hyperlink_to(
                lg.text("sidebar-cloud-source"),
                "https://github.com/matteason/live-cloud-maps",
            );
        }
    }
    fn layer_objects(&mut self, ui: &mut egui::Ui) {
        let lg = self.lang;
        ui.checkbox(&mut self.show_flights, lg.text("layers-flights-toggle"));
        if self.show_flights {
            super::super::aircraft::draw_status(ui, &self.live_aircraft.snapshot(), lg);
            ui.hyperlink_to("OpenSky Network", "https://opensky-network.org/");
            ui.hyperlink_to("ADSB.lol (ODbL)", "https://www.adsb.lol/");
        }
        ui.checkbox(&mut self.show_vessels, lg.text("layers-vessels-toggle"));
        if self.show_vessels {
            ui.small(self.live_vessels.status(lg));
            ui.hyperlink_to("OpenSeaFeed AIS", "https://openseafeed.com/");
            ui.hyperlink_to(
                lg.text("vessels-fallback-source"),
                "https://www.digitraffic.fi/en/marine-traffic/",
            );
        }
        ui.checkbox(
            &mut self.show_satellites,
            lg.text("layers-satellites-toggle"),
        );
        if self.show_satellites {
            ui.small(self.live_satellites.status(lg));
            ui.hyperlink_to("CelesTrak", "https://celestrak.org/");
        }
    }
    fn layer_sky(&mut self, ui: &mut egui::Ui) {
        let lg = self.lang;
        ui.checkbox(&mut self.settings.sky.enabled, lg.text("sky-enabled"));
        ui.checkbox(&mut self.show_atmo, lg.text("layers-atmosphere-toggle"));
        ui.checkbox(&mut self.show_sun, lg.text("layers-sunlight-toggle"));
    }
}

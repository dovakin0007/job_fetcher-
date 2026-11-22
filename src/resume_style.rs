//! Printable resume layouts. Content always comes from the parsed resume
//! (or a per-job selection of those same lines). Output is HTML preview or PDF.

use crate::models::CandidateProfile as Prefs;
use crate::resume::{CandidateProfile, Education, Experience};
use crate::tailored::TailoredResume;
use printpdf::{BuiltinFont, Color, Line, Mm, PdfDocument, PdfDocumentReference, PdfLayerIndex, PdfPageIndex, Point, Rgb};
use serde::{Deserialize, Serialize};
use std::io::BufWriter;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ResumeStyle {
    Classic,
    Modern,
    Compact,
    Timeline,
}

impl ResumeStyle {
    pub fn parse(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "modern" => Self::Modern,
            "compact" => Self::Compact,
            "timeline" => Self::Timeline,
            _ => Self::Classic,
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            Self::Classic => "classic",
            Self::Modern => "modern",
            Self::Compact => "compact",
            Self::Timeline => "timeline",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Classic => "Classic",
            Self::Modern => "Modern",
            Self::Compact => "Compact",
            Self::Timeline => "Timeline",
        }
    }

    pub fn blurb(self) -> &'static str {
        match self {
            Self::Classic => "Serif, centered header, traditional ATS-safe page.",
            Self::Modern => "Bold navy banner, sans-serif, skill chips.",
            Self::Compact => "Two columns: skills and education beside experience.",
            Self::Timeline => "Dates in a left rail, roles as a vertical timeline.",
        }
    }

    pub fn all() -> [Self; 4] {
        [Self::Classic, Self::Modern, Self::Compact, Self::Timeline]
    }
}

pub fn catalog() -> serde_json::Value {
    serde_json::json!({
        "styles": ResumeStyle::all().iter().map(|s| serde_json::json!({
            "id": s.id(),
            "label": s.label(),
            "blurb": s.blurb(),
        })).collect::<Vec<_>>(),
    })
}

#[derive(Debug, Clone, Default)]
pub struct ResumeDoc {
    pub name: String,
    pub headline: String,
    pub contact: String,
    pub summary: String,
    pub skills: Vec<String>,
    pub roles: Vec<DocRole>,
    pub projects: Vec<DocProject>,
    pub education: Vec<Education>,
    pub open_source: Vec<String>,
    pub layout: Vec<String>,
    pub custom_fields: Vec<crate::resume::CustomField>,
}

#[derive(Debug, Clone)]
pub struct DocRole {
    pub title: String,
    pub company: String,
    pub dates: String,
    pub location: String,
    pub bullets: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct DocProject {
    pub name: String,
    pub description: String,
    pub technologies: Vec<String>,
}

impl ResumeDoc {
    pub fn from_profile(profile: &CandidateProfile, prefs: Option<&Prefs>) -> Self {
        Self {
            name: profile
                .name
                .clone()
                .filter(|n| !n.trim().is_empty())
                .or_else(|| prefs.map(|p| p.name.clone()).filter(|n| !n.trim().is_empty()))
                .unwrap_or_else(|| "Candidate".into()),
            headline: profile.headline.clone().unwrap_or_default(),
            contact: contact_line(prefs),
            summary: profile.summary.clone().unwrap_or_default(),
            skills: profile
                .skills
                .iter()
                .map(|s| s.name.clone())
                .filter(|s| !s.trim().is_empty())
                .collect(),
            roles: profile.experience.iter().map(doc_role).collect(),
            projects: profile
                .projects
                .iter()
                .map(|p| DocProject {
                    name: p.name.clone(),
                    description: p.description.clone(),
                    technologies: p.technologies.clone(),
                })
                .collect(),
            education: profile.education.clone(),
            open_source: profile
                .open_source
                .iter()
                .map(|item| format!("{} — {}", item.project, item.contribution))
                .filter(|line| !line.trim().is_empty() && line.trim() != "—")
                .collect(),
            layout: profile.layout.clone(),
            custom_fields: profile.custom_fields.clone(),
        }
    }

    pub fn from_tailored(
        profile: &CandidateProfile,
        prefs: Option<&Prefs>,
        tailored: &TailoredResume,
    ) -> Self {
        Self {
            name: profile
                .name
                .clone()
                .filter(|n| !n.trim().is_empty())
                .or_else(|| prefs.map(|p| p.name.clone()).filter(|n| !n.trim().is_empty()))
                .unwrap_or_else(|| "Candidate".into()),
            headline: tailored.headline.clone(),
            contact: contact_line(prefs),
            summary: tailored.summary.clone(),
            skills: tailored.skills.clone(),
            roles: tailored
                .roles
                .iter()
                .map(|role| DocRole {
                    title: role.title.clone(),
                    company: role.company.clone(),
                    dates: role.dates.clone().unwrap_or_default(),
                    location: role.location.clone().unwrap_or_default(),
                    bullets: role
                        .bullets
                        .iter()
                        .filter(|b| b.selected)
                        .map(|b| b.text.clone())
                        .collect(),
                })
                .filter(|role| !role.bullets.is_empty() || !role.title.trim().is_empty())
                .collect(),
            projects: tailored
                .projects
                .iter()
                .filter(|p| p.selected)
                .map(|p| DocProject {
                    name: p.name.clone(),
                    description: p.description.clone(),
                    technologies: p.technologies.clone(),
                })
                .collect(),
            education: profile.education.clone(),
            open_source: profile
                .open_source
                .iter()
                .map(|item| format!("{} — {}", item.project, item.contribution))
                .filter(|line| !line.trim().is_empty() && line.trim() != "—")
                .collect(),
            layout: profile.layout.clone(),
            custom_fields: profile.custom_fields.clone(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct SectionTitles {
    pub summary: String,
    pub skills: String,
    pub experience: String,
    pub projects: String,
    pub education: String,
    pub open_source: String,
    pub subtitle_pt: f32,
    pub section_pt: f32,
}

impl Default for SectionTitles {
    fn default() -> Self {
        Self {
            summary: "Summary".into(),
            skills: "Skills".into(),
            experience: "Experience".into(),
            projects: "Projects".into(),
            education: "Education".into(),
            open_source: "Open source".into(),
            subtitle_pt: 13.0,
            section_pt: 11.0,
        }
    }
}

impl SectionTitles {
    pub fn parse(
        summary: &str,
        skills: &str,
        experience: &str,
        projects: &str,
        education: &str,
        subtitle_pt: f32,
        section_pt: f32,
    ) -> Self {
        let base = Self::default();
        Self {
            summary: label_or(summary, &base.summary),
            skills: label_or(skills, &base.skills),
            experience: label_or(experience, &base.experience),
            projects: label_or(projects, &base.projects),
            education: label_or(education, &base.education),
            open_source: base.open_source,
            subtitle_pt: if subtitle_pt <= 0.0 { 13.0 } else { subtitle_pt.clamp(10.0, 22.0) },
            section_pt: if section_pt <= 0.0 { 11.0 } else { section_pt.clamp(8.0, 18.0) },
        }
    }

    fn css(&self) -> String {
        format!(
            ".sheet .headline,.sheet .role-head{{font-size:{sub}px;}}\
.sheet.classic .sec h2,.sheet.modern .sec h2,.sheet.compact .sec h2,.sheet.timeline .sec h2{{font-size:{sec}px;}}\
.sec[draggable=true]{{cursor:grab;}}\
.sec.dragging{{opacity:.45;}}",
            sub = self.subtitle_pt,
            sec = self.section_pt,
        )
    }
}

fn label_or(value: &str, fallback: &str) -> String {
    let value = value.trim();
    if value.is_empty() || value.len() > 40 {
        fallback.to_string()
    } else {
        value.to_string()
    }
}

fn contact_line(prefs: Option<&Prefs>) -> String {
    let Some(p) = prefs else {
        return String::new();
    };
    [p.email.trim(), p.phone.trim()]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" · ")
}

fn doc_role(role: &Experience) -> DocRole {
    DocRole {
        title: role.title.clone(),
        company: role.company.clone(),
        dates: role.dates.clone().unwrap_or_default(),
        location: role.location.clone().unwrap_or_default(),
        bullets: role.bullets.iter().map(|b| b.text.clone()).collect(),
    }
}

pub const GOOGLE_FONTS: &[&str] = &[
    "Inter",
    "Roboto",
    "Source Sans 3",
    "IBM Plex Sans",
    "Merriweather",
    "Libre Baskerville",
    "Lora",
    "Playfair Display",
];

#[derive(Clone, Debug)]
pub struct Typeface {
    pub family: String,
    pub body_pt: f32,
    pub heading_pt: f32,
    pub weight: u16,
}

impl Default for Typeface {
    fn default() -> Self {
        Self {
            family: "Inter".into(),
            body_pt: 10.5,
            heading_pt: 26.0,
            weight: 400,
        }
    }
}

impl Typeface {
    pub fn parse(family: &str, body_pt: f32, heading_pt: f32, weight: u16) -> Self {
        let family = GOOGLE_FONTS
            .iter()
            .find(|name| name.eq_ignore_ascii_case(family.trim()))
            .copied()
            .unwrap_or("Inter");
        Self {
            family: family.to_string(),
            body_pt: body_pt.clamp(9.0, 14.0),
            heading_pt: heading_pt.clamp(16.0, 36.0),
            weight: match weight {
                600 => 600,
                700 => 700,
                _ => 400,
            },
        }
    }

    pub fn stylesheet(&self) -> String {
        let family = self.family.replace(' ', "+");
        format!(
            "https://fonts.googleapis.com/css2?family={family}:ital,wght@0,400;0,600;0,700;1,400&display=swap"
        )
    }

    fn css(&self) -> String {
        let fallback = if self.family == "Merriweather"
            || self.family == "Libre Baskerville"
            || self.family == "Lora"
            || self.family == "Playfair Display"
        {
            "serif"
        } else {
            "sans-serif"
        };
        format!(
            ".sheet,.sheet.classic,.sheet.modern,.sheet.compact,.sheet.timeline{{font-family:'{}',{fallback};font-size:{body}pt;font-weight:{weight};}}\
.sheet h1{{font-size:{heading}px;font-weight:700;}}",
            self.family.replace('\'', ""),
            body = self.body_pt,
            weight = self.weight,
            heading = self.heading_pt,
        )
    }
}

/// Downloads a Google Font file (TTF) for embedding in the PDF.
pub async fn download_google_font(family: &str) -> Option<Vec<u8>> {
    let face = Typeface::parse(family, 10.5, 26.0, 400);
    let query = face.family.replace(' ', "+");
    let client = reqwest::Client::new();
    let css = client
        .get(format!("https://fonts.googleapis.com/css?family={query}"))
        .header(
            "User-Agent",
            "Mozilla/5.0 (Windows NT 6.1; rv:40.0) Gecko/20100101 Firefox/40.0",
        )
        .send()
        .await
        .ok()?
        .text()
        .await
        .ok()?;
    let start = css.find("https://fonts.gstatic.com/")?;
    let rest = &css[start..];
    let end = rest.find(')')?;
    let url = rest[..end].trim().trim_matches('"').trim_matches('\'');
    let bytes = client.get(url).send().await.ok()?.bytes().await.ok()?;
    if bytes.len() < 1000 {
        None
    } else {
        Some(bytes.to_vec())
    }
}

pub fn render_html(doc: &ResumeDoc, style: ResumeStyle) -> String {
    render_html_with(doc, style, &Typeface::default(), &SectionTitles::default())
}

pub fn render_html_with(
    doc: &ResumeDoc,
    style: ResumeStyle,
    face: &Typeface,
    titles: &SectionTitles,
) -> String {
    let body = match style {
        ResumeStyle::Classic => html_classic(doc, titles),
        ResumeStyle::Modern => html_modern(doc, titles),
        ResumeStyle::Compact => html_compact(doc, titles),
        ResumeStyle::Timeline => html_timeline(doc, titles),
    };
    format!(
        "<!DOCTYPE html><html lang=\"en\"><head><meta charset=\"utf-8\"><title>{}</title>\
<link rel=\"stylesheet\" href=\"{}\">\
<style>
@page {{ size: A4; margin: 14mm; }}
html,body {{ margin:0; background:#e8edf5; }}
.sheet {{
  width: 210mm; min-height: 297mm; margin: 12px auto; background:#fff; color:#152033;
  box-sizing: border-box; box-shadow: 0 8px 28px rgba(15,23,42,.18);
}}
.sheet * {{ box-sizing: border-box; }}
h1,h2,h3,p,ul {{ margin: 0; }}
ul {{ padding-left: 16px; }}
li {{ margin: 3px 0; }}
.sec {{ margin-top: 12px; }}
.sec h2 {{ font-size: 11px; letter-spacing: .14em; text-transform: uppercase; margin-bottom: 6px; }}
.muted {{ color:#4b5b74; }}
.chips span {{ display:inline-block; border:1px solid #c5d0e0; border-radius:999px; padding:2px 8px; margin:2px 4px 2px 0; font-size:11px; }}
@media print {{ html,body {{ background:#fff; }} .sheet {{ margin:0; box-shadow:none; }} }}
{css}
{type_css}
{title_css}
</style></head><body>{body}{script}</body></html>",
        escape(&doc.name),
        face.stylesheet(),
        css = style_css(style),
        type_css = face.css(),
        title_css = titles.css(),
        body = body,
        script = drag_script(),
    )
}

fn drag_script() -> String {
    r#"<script>
(function(){
  if (!window.frameElement || window.frameElement.classList.contains("design-thumb")) return;
  var drag = null;
  document.querySelectorAll("[data-block]").forEach(function(el){
    el.setAttribute("draggable","true");
    el.addEventListener("dragstart", function(){ drag = el; el.classList.add("dragging"); });
    el.addEventListener("dragend", function(){
      el.classList.remove("dragging");
      var order = Array.prototype.map.call(document.querySelectorAll("[data-block]"), function(n){ return n.getAttribute("data-block"); });
      parent.postMessage({ type: "resume-layout", order: order }, "*");
    });
    el.addEventListener("dragover", function(e){
      e.preventDefault();
      if (!drag || drag === el || !el.parentNode || drag.parentNode !== el.parentNode) return;
      var box = el.getBoundingClientRect();
      var after = e.clientY > box.top + box.height / 2;
      el.parentNode.insertBefore(drag, after ? el.nextSibling : el);
    });
  });
})();
</script>"#
        .to_string()
}

fn style_css(style: ResumeStyle) -> &'static str {
    match style {
        ResumeStyle::Classic => "
.sheet.classic { padding: 18mm 18mm 16mm; font: 10.5pt/1.45 'Times New Roman', Times, serif; }
.classic header { text-align:center; border-bottom: 1.5px solid #152033; padding-bottom: 8px; margin-bottom: 10px; }
.classic h1 { font-size: 26px; letter-spacing: .04em; }
.classic .headline { font-style: italic; margin-top: 4px; }
.classic .contact { margin-top: 4px; font-size: 10.5px; }
.classic .role-head { display:flex; justify-content:space-between; gap:12px; font-weight:700; }
.classic .sec h2 { border-bottom: 1px solid #c9d3e2; padding-bottom: 3px; }
",
        ResumeStyle::Modern => "
.sheet.modern { padding: 0 0 16mm; font: 10pt/1.45 Inter, 'Segoe UI', sans-serif; }
.modern .banner { background:#152033; color:#f4f7ff; padding: 16mm 16mm 12mm; }
.modern h1 { font-size: 28px; letter-spacing: .01em; }
.modern .headline { color:#c5d4f5; margin-top: 4px; }
.modern .contact { color:#9aadc9; margin-top: 6px; font-size: 11px; }
.modern .inner { padding: 12mm 16mm 0; }
.modern .sec h2 { color:#3d4fce; border-bottom: 2px solid #3d4fce; padding-bottom: 3px; }
.modern .role-head { display:flex; justify-content:space-between; gap:12px; font-weight:700; }
.modern .chips span { background:#eef2ff; border-color:#c7d2fe; color:#1e2a5a; }
",
        ResumeStyle::Compact => "
.sheet.compact { padding: 14mm; font: 9.5pt/1.4 Inter, 'Segoe UI', sans-serif; }
.compact header { margin-bottom: 10px; }
.compact h1 { font-size: 22px; }
.compact .grid { display:grid; grid-template-columns: 68mm 1fr; gap: 12mm; }
.compact aside { border-right: 1px solid #d5deea; padding-right: 10mm; }
.compact .sec h2 { color:#1d4ed8; }
.compact .role-head { font-weight:700; }
",
        ResumeStyle::Timeline => "
.sheet.timeline { padding: 16mm 16mm 14mm; font: 10pt/1.45 Inter, 'Segoe UI', sans-serif; }
.timeline header { border-left: 6px solid #0f766e; padding-left: 12px; margin-bottom: 14px; }
.timeline h1 { font-size: 24px; }
.timeline .row { display:grid; grid-template-columns: 28mm 1fr; gap: 10px; margin: 10px 0; }
.timeline .when { font-size: 10px; color:#0f766e; font-weight:700; padding-top: 2px; }
.timeline .rail { border-left: 2px solid #99f6e4; padding-left: 12px; }
.timeline .sec h2 { color:#0f766e; }
",
    }
}

fn html_classic(doc: &ResumeDoc, titles: &SectionTitles) -> String {
    format!(
        "<div class=\"sheet classic\"><header><h1>{}</h1>{}{}</header><div class=\"blocks\">{}</div></div>",
        escape(&doc.name),
        maybe_p("headline muted", &doc.headline),
        maybe_p("contact muted", &doc.contact),
        flow_blocks(doc, titles, false, false, false),
    )
}

fn html_modern(doc: &ResumeDoc, titles: &SectionTitles) -> String {
    format!(
        "<div class=\"sheet modern\"><div class=\"banner\"><h1>{}</h1>{}{}</div><div class=\"inner blocks\">{}</div></div>",
        escape(&doc.name),
        maybe_p("headline", &doc.headline),
        maybe_p("contact", &doc.contact),
        flow_blocks(doc, titles, true, false, false),
    )
}

fn html_compact(doc: &ResumeDoc, titles: &SectionTitles) -> String {
    let mut aside = String::new();
    let mut main = String::new();
    for id in layout_ids(doc) {
        let html = block_html(doc, titles, &id, true, true, false);
        if id == "skills" || id == "education" {
            aside.push_str(&html);
        } else {
            main.push_str(&html);
        }
    }
    if !doc.contact.trim().is_empty() {
        aside.push_str(&section(
            "contact",
            "Contact",
            format!("<p>{}</p>", escape(&doc.contact)),
        ));
    }
    format!(
        "<div class=\"sheet compact\"><header><h1>{}</h1>{}</header><div class=\"grid\"><aside class=\"blocks\">{}</aside><main class=\"blocks\">{}</main></div></div>",
        escape(&doc.name),
        maybe_p("headline muted", &doc.headline),
        aside,
        main
    )
}

fn html_timeline(doc: &ResumeDoc, titles: &SectionTitles) -> String {
    format!(
        "<div class=\"sheet timeline\"><header><h1>{}</h1>{}{}</header><div class=\"blocks\">{}</div></div>",
        escape(&doc.name),
        maybe_p("headline muted", &doc.headline),
        maybe_p("contact muted", &doc.contact),
        flow_blocks(doc, titles, true, false, true),
    )
}

fn flow_blocks(doc: &ResumeDoc, titles: &SectionTitles, chips: bool, stacked: bool, timeline: bool) -> String {
    layout_ids(doc)
        .into_iter()
        .map(|id| block_html(doc, titles, &id, chips, stacked, timeline))
        .collect()
}

fn layout_ids(doc: &ResumeDoc) -> Vec<String> {
    const BASE: [&str; 6] = [
        "summary",
        "skills",
        "experience",
        "projects",
        "education",
        "open_source",
    ];
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for id in &doc.layout {
        let known = BASE.contains(&id.as_str()) || doc.custom_fields.iter().any(|f| f.id == *id);
        if known && seen.insert(id.clone()) {
            out.push(id.clone());
        }
    }
    for id in BASE {
        if seen.insert(id.to_string()) {
            out.push(id.to_string());
        }
    }
    for field in &doc.custom_fields {
        if !field.id.is_empty() && seen.insert(field.id.clone()) {
            out.push(field.id.clone());
        }
    }
    out
}

fn block_html(
    doc: &ResumeDoc,
    titles: &SectionTitles,
    id: &str,
    chips: bool,
    stacked: bool,
    timeline: bool,
) -> String {
    match id {
        "summary" => section("summary", &titles.summary, maybe_p("", &doc.summary)),
        "skills" => skills_block(&doc.skills, chips, &titles.skills),
        "experience" if timeline => timeline_roles(doc, &titles.experience),
        "experience" => roles_block(&doc.roles, stacked, &titles.experience),
        "projects" => projects_block(&doc.projects, &titles.projects),
        "education" => education_block(&doc.education, &titles.education),
        "open_source" => lines_block(&doc.open_source, &titles.open_source),
        other => doc
            .custom_fields
            .iter()
            .find(|field| field.id == other)
            .map(|field| {
                let title = if field.label.trim().is_empty() { "Custom" } else { field.label.as_str() };
                let inner = if field.body.trim().is_empty() {
                    "<p class=\"muted\">&nbsp;</p>".to_string()
                } else {
                    maybe_p("", &field.body)
                };
                section(&field.id, title, inner)
            })
            .unwrap_or_default(),
    }
}

fn timeline_roles(doc: &ResumeDoc, title: &str) -> String {
    if doc.roles.is_empty() {
        return String::new();
    }
    let mut roles = String::new();
    for role in &doc.roles {
        roles.push_str(&format!(
            "<div class=\"row\"><div class=\"when\">{}</div><div class=\"rail\"><div class=\"role-head\">{}{}</div>{}{}</div></div>",
            escape(&role.dates),
            escape(&role.title),
            if role.company.trim().is_empty() { String::new() } else { format!(" · {}", escape(&role.company)) },
            if role.location.trim().is_empty() { String::new() } else { format!("<p class=\"muted\">{}</p>", escape(&role.location)) },
            bullets_html(&role.bullets),
        ));
    }
    section("experience", title, roles)
}

fn maybe_p(class: &str, text: &str) -> String {
    if text.trim().is_empty() {
        String::new()
    } else if class.is_empty() {
        format!("<p>{}</p>", escape(text))
    } else {
        format!("<p class=\"{class}\">{}</p>", escape(text))
    }
}

fn section(id: &str, title: &str, inner: String) -> String {
    if inner.trim().is_empty() {
        String::new()
    } else {
        format!(
            "<div class=\"sec\" data-block=\"{}\"><h2>{}</h2>{inner}</div>",
            escape(id),
            escape(title)
        )
    }
}

fn skills_block(skills: &[String], chips: bool, title: &str) -> String {
    if skills.is_empty() {
        return String::new();
    }
    if chips {
        let inner = skills
            .iter()
            .map(|s| format!("<span>{}</span>", escape(s)))
            .collect::<String>();
        section("skills", title, format!("<div class=\"chips\">{inner}</div>"))
    } else {
        section(
            "skills",
            title,
            format!("<p>{}</p>", escape(&skills.join(" · "))),
        )
    }
}

fn roles_block(roles: &[DocRole], stacked: bool, title: &str) -> String {
    if roles.is_empty() {
        return String::new();
    }
    let mut inner = String::new();
    for role in roles {
        let head = if stacked {
            format!(
                "<div class=\"role-head\">{}{}</div><p class=\"muted\">{}{}</p>",
                escape(&role.title),
                if role.company.trim().is_empty() {
                    String::new()
                } else {
                    format!(" · {}", escape(&role.company))
                },
                escape(&role.dates),
                if role.location.trim().is_empty() {
                    String::new()
                } else {
                    format!(" · {}", escape(&role.location))
                }
            )
        } else {
            format!(
                "<div class=\"role-head\"><span>{}{}</span><span class=\"muted\">{}</span></div>{}",
                escape(&role.title),
                if role.company.trim().is_empty() {
                    String::new()
                } else {
                    format!(" · {}", escape(&role.company))
                },
                escape(&role.dates),
                if role.location.trim().is_empty() {
                    String::new()
                } else {
                    format!("<p class=\"muted\">{}</p>", escape(&role.location))
                }
            )
        };
        inner.push_str(&format!(
            "<div style=\"margin:8px 0 10px\">{head}{}</div>",
            bullets_html(&role.bullets)
        ));
    }
    section("experience", title, inner)
}

fn bullets_html(items: &[String]) -> String {
    if items.is_empty() {
        return String::new();
    }
    let lis = items
        .iter()
        .filter(|t| !t.trim().is_empty())
        .map(|t| format!("<li>{}</li>", escape(t)))
        .collect::<String>();
    if lis.is_empty() {
        String::new()
    } else {
        format!("<ul>{lis}</ul>")
    }
}

fn projects_block(projects: &[DocProject], title: &str) -> String {
    if projects.is_empty() {
        return String::new();
    }
    let mut inner = String::new();
    for p in projects {
        inner.push_str(&format!(
            "<div style=\"margin:8px 0\"><strong>{}</strong><p>{}</p>{}</div>",
            escape(&p.name),
            escape(&p.description),
            if p.technologies.is_empty() {
                String::new()
            } else {
                format!("<p class=\"muted\">{}</p>", escape(&p.technologies.join(", ")))
            }
        ));
    }
    section("projects", title, inner)
}

fn education_block(items: &[Education], title: &str) -> String {
    if items.is_empty() {
        return String::new();
    }
    let mut inner = String::new();
    for e in items {
        inner.push_str(&format!(
            "<p><strong>{}</strong>{}{}</p>",
            escape(&e.qualification),
            if e.institution.trim().is_empty() {
                String::new()
            } else {
                format!(" · {}", escape(&e.institution))
            },
            e.dates
                .as_deref()
                .filter(|d| !d.trim().is_empty())
                .map(|d| format!(" <span class=\"muted\">({})</span>", escape(d)))
                .unwrap_or_default()
        ));
    }
    section("education", title, inner)
}

fn lines_block(lines: &[String], title: &str) -> String {
    if lines.is_empty() {
        return String::new();
    }
    let inner = lines
        .iter()
        .map(|line| format!("<p>{}</p>", escape(line)))
        .collect();
    section("open_source", title, inner)
}

fn escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

pub fn render_pdf(doc: &ResumeDoc, style: ResumeStyle) -> anyhow::Result<Vec<u8>> {
    render_pdf_with(doc, style, &Typeface::default(), &SectionTitles::default(), None)
}

pub fn render_pdf_with(
    doc: &ResumeDoc,
    style: ResumeStyle,
    face: &Typeface,
    titles: &SectionTitles,
    font_bytes: Option<&[u8]>,
) -> anyhow::Result<Vec<u8>> {
    let (pdf, page, layer) = PdfDocument::new(&doc.name, Mm(210.0), Mm(297.0), "Layer 1");
    let serif = style == ResumeStyle::Classic;
    let (regular, bold) = if let Some(bytes) = font_bytes {
        let regular = pdf.add_external_font(std::io::Cursor::new(bytes))?;
        let bold = pdf.add_external_font(std::io::Cursor::new(bytes))?;
        (regular, bold)
    } else {
        let regular = pdf.add_builtin_font(if serif {
            BuiltinFont::TimesRoman
        } else {
            BuiltinFont::Helvetica
        })?;
        let bold = pdf.add_builtin_font(if serif {
            BuiltinFont::TimesBold
        } else {
            BuiltinFont::HelveticaBold
        })?;
        (regular, bold)
    };
    let mut c = Canvas {
        doc: pdf,
        page,
        layer,
        y: 277.0,
        regular,
        bold,
        left: 18.0,
        width: 174.0,
        scale: face.body_pt / 10.5,
        name_pt: face.heading_pt * 22.0 / 26.0,
        titles: titles.clone(),
    };
    match style {
        ResumeStyle::Classic => paint_classic(&mut c, doc),
        ResumeStyle::Modern => paint_modern(&mut c, doc),
        ResumeStyle::Compact => paint_compact(&mut c, doc),
        ResumeStyle::Timeline => paint_timeline(&mut c, doc),
    }
    let mut buf = BufWriter::new(Vec::new());
    c.doc.save(&mut buf)?;
    Ok(buf.into_inner()?)
}

struct Canvas {
    doc: PdfDocumentReference,
    page: PdfPageIndex,
    layer: PdfLayerIndex,
    y: f32,
    regular: printpdf::IndirectFontRef,
    bold: printpdf::IndirectFontRef,
    left: f32,
    width: f32,
    scale: f32,
    name_pt: f32,
    titles: SectionTitles,
}

impl Canvas {
    fn layer(&self) -> printpdf::PdfLayerReference {
        self.doc.get_page(self.page).get_layer(self.layer)
    }

    fn new_page(&mut self) {
        let (page, layer) = self.doc.add_page(Mm(210.0), Mm(297.0), "Layer 1");
        self.page = page;
        self.layer = layer;
        self.y = 277.0;
    }

    fn need(&mut self, mm: f32) {
        if self.y - mm < 16.0 {
            self.new_page();
        }
    }

    fn fill_rect(&self, x: f32, y: f32, w: f32, h: f32, r: f32, g: f32, b: f32) {
        let layer = self.layer();
        layer.set_fill_color(Color::Rgb(Rgb::new(r, g, b, None)));
        layer.set_outline_color(Color::Rgb(Rgb::new(r, g, b, None)));
        layer.set_outline_thickness(0.0);
        let line = Line {
            points: vec![
                (Point::new(Mm(x), Mm(y)), false),
                (Point::new(Mm(x + w), Mm(y)), false),
                (Point::new(Mm(x + w), Mm(y + h)), false),
                (Point::new(Mm(x), Mm(y + h)), false),
            ],
            is_closed: true,
        };
        layer.add_line(line);
    }

    fn rule(&self, x: f32, y: f32, w: f32, r: f32, g: f32, b: f32) {
        let layer = self.layer();
        layer.set_outline_color(Color::Rgb(Rgb::new(r, g, b, None)));
        layer.set_outline_thickness(0.7);
        let line = Line {
            points: vec![
                (Point::new(Mm(x), Mm(y)), false),
                (Point::new(Mm(x + w), Mm(y)), false),
            ],
            is_closed: false,
        };
        layer.add_line(line);
    }

    fn text(&self, font: &printpdf::IndirectFontRef, size: f32, x: f32, y: f32, text: &str) {
        let size = size * self.scale;
        self.layer()
            .use_text(winansi(text), size, Mm(x), Mm(y), font);
    }

    fn wrapped(&mut self, font: &printpdf::IndirectFontRef, size: f32, x: f32, width: f32, text: &str) {
        let size = size * self.scale;
        let lines = wrap(text, width, size);
        let step = size * 0.42;
        for line in lines {
            self.need(step + 1.0);
            self.text(font, size, x, self.y, &line);
            self.y -= step;
        }
    }

    fn heading(&mut self, title: &str, accent: (f32, f32, f32)) {
        self.need(10.0);
        self.y -= 3.0;
        self.text(&self.bold.clone(), self.titles.section_pt, self.left, self.y, title);
        self.y -= 2.0;
        self.rule(self.left, self.y, self.width, accent.0, accent.1, accent.2);
        self.y -= 5.0;
    }
}

fn paint_classic(c: &mut Canvas, doc: &ResumeDoc) {
    c.need(20.0);
    let name = winansi(&doc.name);
    let name_w = estimate(&name, c.name_pt);
    c.text(
        &c.bold.clone(),
        c.name_pt,
        (210.0 - name_w).max(c.left) / 2.0,
        c.y,
        &name,
    );
    c.y -= 8.0;
    if !doc.headline.trim().is_empty() {
        let h = winansi(&doc.headline);
        let w = estimate(&h, c.titles.subtitle_pt);
        c.text(&c.regular.clone(), c.titles.subtitle_pt, (210.0 - w).max(c.left) / 2.0, c.y, &h);
        c.y -= 5.5;
    }
    if !doc.contact.trim().is_empty() {
        let h = winansi(&doc.contact);
        let w = estimate(&h, 9.0);
        c.text(&c.regular.clone(), 9.0, (210.0 - w).max(c.left) / 2.0, c.y, &h);
        c.y -= 5.0;
    }
    c.rule(c.left, c.y, c.width, 0.08, 0.12, 0.2);
    c.y -= 7.0;
    body_flow(c, doc, (0.08, 0.12, 0.2), false);
}

fn paint_modern(c: &mut Canvas, doc: &ResumeDoc) {
    c.fill_rect(0.0, 255.0, 210.0, 42.0, 0.08, 0.12, 0.2);
    c.left = 16.0;
    c.width = 178.0;
    c.y = 284.0;
    c.layer()
        .set_fill_color(Color::Rgb(Rgb::new(0.96, 0.97, 1.0, None)));
    c.text(&c.bold.clone(), c.name_pt, c.left, c.y, &winansi(&doc.name));
    c.y -= 8.0;
    if !doc.headline.trim().is_empty() {
        c.text(&c.regular.clone(), c.titles.subtitle_pt, c.left, c.y, &winansi(&doc.headline));
        c.y -= 5.5;
    }
    if !doc.contact.trim().is_empty() {
        c.text(&c.regular.clone(), 9.0, c.left, c.y, &winansi(&doc.contact));
    }
    c.y = 248.0;
    body_flow(c, doc, (0.24, 0.31, 0.81), true);
}

fn paint_compact(c: &mut Canvas, doc: &ResumeDoc) {
    c.left = 14.0;
    c.width = 182.0;
    c.text(&c.bold.clone(), c.name_pt, c.left, c.y, &winansi(&doc.name));
    c.y -= 7.0;
    if !doc.headline.trim().is_empty() {
        c.text(&c.regular.clone(), c.titles.subtitle_pt, c.left, c.y, &winansi(&doc.headline));
        c.y -= 6.0;
    }
    let accent = (0.11, 0.31, 0.85);
    let order = layout_ids(doc);
    let split = 72.0;
    let top = c.y;
    let left_w = 52.0;
    let right_x = c.left + split;
    let right_w = 210.0 - 14.0 - right_x;
    // Left column
    let saved_left = c.left;
    let saved_width = c.width;
    c.width = left_w;
    for id in order.iter().filter(|id| id.as_str() == "skills" || id.as_str() == "education") {
        paint_section(c, doc, id, accent, true, true, false);
    }
    if !doc.contact.trim().is_empty() {
        c.heading("CONTACT", (0.11, 0.31, 0.85));
        c.wrapped(&c.regular.clone(), 8.5, c.left, left_w, &doc.contact);
    }
    let left_end = c.y;
    // Right column
    c.left = right_x;
    c.width = right_w;
    c.y = top;
    for id in order.iter().filter(|id| id.as_str() != "skills" && id.as_str() != "education") {
        paint_section(c, doc, id, accent, true, true, false);
    }
    c.y = c.y.min(left_end);
    c.left = saved_left;
    c.width = saved_width;
}

fn paint_timeline(c: &mut Canvas, doc: &ResumeDoc) {
    c.fill_rect(c.left - 2.0, c.y - 2.0, 2.2, 16.0, 0.06, 0.46, 0.43);
    c.text(&c.bold.clone(), c.name_pt, c.left + 6.0, c.y, &winansi(&doc.name));
    c.y -= 7.0;
    if !doc.headline.trim().is_empty() {
        c.text(
            &c.regular.clone(),
            c.titles.subtitle_pt,
            c.left + 6.0,
            c.y,
            &winansi(&doc.headline),
        );
        c.y -= 5.5;
    }
    if !doc.contact.trim().is_empty() {
        c.text(
            &c.regular.clone(),
            9.0,
            c.left + 6.0,
            c.y,
            &winansi(&doc.contact),
        );
        c.y -= 6.0;
    }
    let accent = (0.06, 0.46, 0.43);
    for id in layout_ids(doc) {
        paint_section(c, doc, &id, accent, true, false, true);
    }
}

fn body_flow(c: &mut Canvas, doc: &ResumeDoc, accent: (f32, f32, f32), chip_skills: bool) {
    for id in layout_ids(doc) {
        paint_section(c, doc, &id, accent, chip_skills, false, false);
    }
}

fn paint_section(
    c: &mut Canvas,
    doc: &ResumeDoc,
    id: &str,
    accent: (f32, f32, f32),
    chips: bool,
    stacked: bool,
    timeline: bool,
) {
    let custom = doc.custom_fields.iter().find(|field| field.id == id).cloned();
    let label = match id {
        "summary" => c.titles.summary.clone(),
        "skills" => c.titles.skills.clone(),
        "experience" => c.titles.experience.clone(),
        "projects" => c.titles.projects.clone(),
        "education" => c.titles.education.clone(),
        "open_source" => c.titles.open_source.clone(),
        _ => custom.as_ref().map(|field| field.label.clone()).unwrap_or_default(),
    };
    let label = if label.trim().is_empty() { "Custom".into() } else { label.to_uppercase() };
    match id {
        "summary" if !doc.summary.trim().is_empty() => {
            c.heading(&label, accent);
            c.wrapped(&c.regular.clone(), 10.0, c.left, c.width, &doc.summary);
        }
        "skills" if !doc.skills.is_empty() => {
            c.heading(&label, accent);
            let joined = if chips { doc.skills.join("   ") } else { doc.skills.join(" · ") };
            c.wrapped(&c.regular.clone(), if chips { 9.5 } else { 10.0 }, c.left, c.width, &joined);
        }
        "experience" if !doc.roles.is_empty() && timeline => {
            c.heading(&label, accent);
            let rail = c.left;
            let text_x = c.left + 32.0;
            let text_w = c.width - 32.0;
            let subtitle = c.titles.subtitle_pt;
            for role in &doc.roles {
                c.need(14.0);
                c.text(&c.bold.clone(), 8.5, rail, c.y, &winansi(&role.dates));
                let title = format!(
                    "{}{}",
                    role.title,
                    if role.company.trim().is_empty() { String::new() } else { format!(" · {}", role.company) }
                );
                c.wrapped(&c.bold.clone(), subtitle, text_x, text_w, &title);
                if !role.location.trim().is_empty() {
                    c.wrapped(&c.regular.clone(), 8.5, text_x, text_w, &role.location);
                }
                for bullet in &role.bullets {
                    c.wrapped(&c.regular.clone(), 9.5, text_x, text_w, &format!("• {bullet}"));
                }
                c.y -= 3.0;
            }
        }
        "experience" if !doc.roles.is_empty() => {
            c.heading(&label, accent);
            for role in &doc.roles {
                write_role(c, role, stacked);
            }
        }
        "projects" if !doc.projects.is_empty() => {
            c.heading(&label, accent);
            for project in &doc.projects {
                write_project(c, project);
            }
        }
        "education" if !doc.education.is_empty() => {
            c.heading(&label, accent);
            for item in &doc.education {
                let line = format!(
                    "{}{}{}",
                    item.qualification,
                    if item.institution.trim().is_empty() { String::new() } else { format!(", {}", item.institution) },
                    item.dates.as_deref().filter(|d| !d.trim().is_empty()).map(|d| format!(" ({d})")).unwrap_or_default()
                );
                c.wrapped(&c.regular.clone(), 10.0, c.left, c.width, &line);
            }
        }
        "open_source" if !doc.open_source.is_empty() => {
            c.heading(&label, accent);
            for line in &doc.open_source {
                c.wrapped(&c.regular.clone(), 9.5, c.left, c.width, line);
            }
        }
        _ => {
            if let Some(field) = custom {
                if !field.body.trim().is_empty() {
                    c.heading(&label, accent);
                    c.wrapped(&c.regular.clone(), 10.0, c.left, c.width, &field.body);
                }
            }
        }
    }
}

fn write_role(c: &mut Canvas, role: &DocRole, stacked: bool) {
    c.need(12.0);
    let title = format!(
        "{}{}",
        role.title,
        if role.company.trim().is_empty() {
            String::new()
        } else {
            format!(" · {}", role.company)
        }
    );
    let title_size = c.titles.subtitle_pt;
    if stacked {
        c.wrapped(&c.bold.clone(), title_size, c.left, c.width, &title);
        let meta = format!(
            "{}{}{}",
            role.dates,
            if !role.dates.trim().is_empty() && !role.location.trim().is_empty() {
                " · "
            } else {
                ""
            },
            role.location
        );
        if !meta.trim().is_empty() {
            c.wrapped(&c.regular.clone(), 8.5, c.left, c.width, &meta);
        }
    } else {
        c.text(&c.bold.clone(), title_size, c.left, c.y, &winansi(&title));
        if !role.dates.trim().is_empty() {
            let d = winansi(&role.dates);
            let w = estimate(&d, 9.0);
            c.text(&c.regular.clone(), 9.0, c.left + c.width - w, c.y, &d);
        }
        c.y -= 4.8;
        if !role.location.trim().is_empty() {
            c.wrapped(&c.regular.clone(), 8.5, c.left, c.width, &role.location);
        }
    }
    for bullet in &role.bullets {
        c.wrapped(&c.regular.clone(), 9.5, c.left, c.width, &format!("• {bullet}"));
    }
    c.y -= 3.0;
}

fn write_project(c: &mut Canvas, p: &DocProject) {
    c.wrapped(&c.bold.clone(), 10.5, c.left, c.width, &p.name);
    if !p.description.trim().is_empty() {
        c.wrapped(&c.regular.clone(), 9.5, c.left, c.width, &p.description);
    }
    if !p.technologies.is_empty() {
        c.wrapped(
            &c.regular.clone(),
            8.5,
            c.left,
            c.width,
            &p.technologies.join(", "),
        );
    }
    c.y -= 2.0;
}

fn winansi(text: &str) -> String {
    text.chars()
        .map(|ch| match ch {
            '–' | '—' => '-',
            '’' | '‘' => '\'',
            '“' | '”' => '"',
            '•' => '-',
            c if (c as u32) <= 255 => c,
            _ => '?',
        })
        .collect()
}

fn estimate(text: &str, size_pt: f32) -> f32 {
    text.chars().count() as f32 * size_pt * 0.47 * 0.352778
}

fn wrap(text: &str, width_mm: f32, size_pt: f32) -> Vec<String> {
    let cleaned = winansi(text);
    let max = (width_mm / (size_pt * 0.47 * 0.352778)).max(12.0) as usize;
    let mut lines = Vec::new();
    let mut current = String::new();
    for word in cleaned.split_whitespace() {
        let next = if current.is_empty() {
            word.to_string()
        } else {
            format!("{current} {word}")
        };
        if next.chars().count() > max && !current.is_empty() {
            lines.push(current);
            current = word.to_string();
        } else {
            current = next;
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

pub fn pdf_response(bytes: Vec<u8>, filename: &str) -> ([(axum::http::header::HeaderName, String); 2], Vec<u8>) {
    (
        [
            (
                axum::http::header::CONTENT_TYPE,
                "application/pdf".to_string(),
            ),
            (
                axum::http::header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{filename}\""),
            ),
        ],
        bytes,
    )
}

pub fn safe_filename(name: &str) -> String {
    let stem: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c
            } else {
                '-'
            }
        })
        .collect();
    let stem = stem.trim_matches('-');
    if stem.is_empty() {
        "resume.pdf".into()
    } else {
        format!("{stem}.pdf")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resume::{EvidenceBullet, SkillEvidence};

    fn sample() -> CandidateProfile {
        CandidateProfile {
            name: Some("Gowtham".into()),
            headline: Some("Backend engineer".into()),
            summary: Some("Builds data systems in ClickHouse.".into()),
            skills: vec![SkillEvidence {
                name: "ClickHouse".into(),
                evidence: vec![],
                confidence: 0.9,
            }],
            experience: vec![Experience {
                company: "Acme".into(),
                title: "Engineer".into(),
                dates: Some("2022 – Present".into()),
                location: Some("Bengaluru".into()),
                bullets: vec![EvidenceBullet {
                    text: "Maintained ClickHouse tables.".into(),
                    source_excerpt: "Maintained ClickHouse tables.".into(),
                }],
            }],
            projects: vec![],
            education: vec![],
            open_source: vec![], layout: vec![], custom_fields: vec![],
            source_file: None,
            extraction_warnings: vec![],
        }
    }

    #[test]
    fn every_style_emits_pdf_and_named_html() {
        let doc = ResumeDoc::from_profile(&sample(), None);
        for style in ResumeStyle::all() {
            let html = render_html(&doc, style);
            assert!(html.contains("Gowtham"), "{}", style.id());
            assert!(html.contains("ClickHouse"), "{}", style.id());
            let pdf = render_pdf(&doc, style).expect(style.id());
            assert!(pdf.starts_with(b"%PDF"), "{}", style.id());
        }
    }
}

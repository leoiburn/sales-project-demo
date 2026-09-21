//! WHAT: generates an ADF 1.0 XML lead document for dealership CRMs.
//! WHY:  ADF is what DealerSocket, VinSolutions, eLeads and the rest ingest. A
//!       lead that does not parse is a lead the dealership never sees, and a
//!       customer whose name contains an ampersand is not an edge case - it is
//!       Tuesday.
//! HOW:  every node is written through quick-xml's event writer, which escapes
//!       text for us. There is no string concatenation anywhere in this file,
//!       and that is the point: concatenation is how `Jones & Sons` becomes a
//!       parse error.
//!
//!       ADF 1.0 has no node for an appointment, so the booked time goes into
//!       <comments> along with the summary.

use anyhow::Result;
use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use quick_xml::events::{BytesDecl, BytesPI, BytesStart, BytesText, Event};
use quick_xml::Writer;
use std::io::Cursor;

use crate::db::Vehicle;

pub const PROVIDER_NAME: &str = "Automotrix AI Sales Assistant";

#[derive(Debug, Clone)]
pub struct AdfLead {
    pub lead_id: String,
    pub requested_at: DateTime<Utc>,
    pub timezone: Tz,
    pub vehicles: Vec<Vehicle>,
    pub first_name: Option<String>,
    pub last_name: Option<String>,
    pub email: Option<String>,
    pub phone: Option<String>,
    pub comments: String,
    pub dealer_name: String,
}

fn open(w: &mut Writer<Cursor<Vec<u8>>>, name: &str, attrs: &[(&str, &str)]) -> Result<()> {
    let mut start = BytesStart::new(name);
    for (k, v) in attrs {
        start.push_attribute((*k, *v));
    }
    w.write_event(Event::Start(start))?;
    Ok(())
}

fn close(w: &mut Writer<Cursor<Vec<u8>>>, name: &str) -> Result<()> {
    w.write_event(Event::End(quick_xml::events::BytesEnd::new(name)))?;
    Ok(())
}

/// One element with text content. BytesText::new escapes on write, so `<`, `&`,
/// quotes, accents and emoji all come out well-formed.
fn leaf(
    w: &mut Writer<Cursor<Vec<u8>>>,
    name: &str,
    attrs: &[(&str, &str)],
    text: &str,
) -> Result<()> {
    open(w, name, attrs)?;
    w.write_event(Event::Text(BytesText::new(text)))?;
    close(w, name)
}

/// ADF's vehicle status vocabulary, derived from our own condition column.
fn adf_status(condition: &str) -> &'static str {
    match condition {
        "new" => "new",
        // ADF 1.0 has no "cpo" - certified cars are used cars with a warranty
        _ => "used",
    }
}

pub fn render(lead: &AdfLead) -> Result<String> {
    let mut w = Writer::new_with_indent(Cursor::new(Vec::new()), b' ', 2);

    w.write_event(Event::Decl(BytesDecl::new("1.0", None, None)))?;
    w.write_event(Event::Text(BytesText::from_escaped("\n")))?;
    w.write_event(Event::PI(BytesPI::new("adf version=\"1.0\"")))?;
    w.write_event(Event::Text(BytesText::from_escaped("\n")))?;

    open(&mut w, "adf", &[])?;
    open(&mut w, "prospect", &[("status", "new")])?;

    leaf(&mut w, "id", &[("sequence", "1"), ("source", PROVIDER_NAME)], &lead.lead_id)?;
    leaf(
        &mut w,
        "requestdate",
        &[],
        &lead
            .requested_at
            .with_timezone(&lead.timezone)
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, false),
    )?;

    for v in &lead.vehicles {
        open(
            &mut w,
            "vehicle",
            &[("interest", "buy"), ("status", adf_status(&v.condition))],
        )?;
        leaf(&mut w, "year", &[], &v.year.to_string())?;
        leaf(&mut w, "make", &[], &v.make)?;
        leaf(&mut w, "model", &[], &v.model)?;
        if let Some(trim) = &v.trim_level {
            leaf(&mut w, "trim", &[], trim)?;
        }
        leaf(&mut w, "vin", &[], &v.vin)?;
        leaf(&mut w, "stock", &[], &v.stock_number)?;
        leaf(&mut w, "odometer", &[("units", "mi")], &v.mileage.to_string())?;
        // ADF prices are plain decimal, no currency symbol
        leaf(
            &mut w,
            "price",
            &[("type", "asking"), ("currency", "USD")],
            &format!("{}.{:02}", v.list_price_cents / 100, v.list_price_cents % 100),
        )?;
        close(&mut w, "vehicle")?;
    }

    open(&mut w, "customer", &[])?;
    open(&mut w, "contact", &[])?;
    if let Some(first) = &lead.first_name {
        leaf(&mut w, "name", &[("part", "first")], first)?;
    }
    if let Some(last) = &lead.last_name {
        leaf(&mut w, "name", &[("part", "last")], last)?;
    }
    if let Some(email) = &lead.email {
        leaf(&mut w, "email", &[], email)?;
    }
    if let Some(phone) = &lead.phone {
        leaf(&mut w, "phone", &[("type", "voice")], phone)?;
    }
    close(&mut w, "contact")?;
    leaf(&mut w, "comments", &[], &lead.comments)?;
    close(&mut w, "customer")?;

    open(&mut w, "vendor", &[])?;
    leaf(&mut w, "vendorname", &[], &lead.dealer_name)?;
    close(&mut w, "vendor")?;

    open(&mut w, "provider", &[])?;
    leaf(&mut w, "name", &[("part", "full")], PROVIDER_NAME)?;
    close(&mut w, "provider")?;

    close(&mut w, "prospect")?;
    close(&mut w, "adf")?;

    Ok(String::from_utf8(w.into_inner().into_inner())? + "\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn vehicle() -> Vehicle {
        Vehicle {
            id: Uuid::nil(),
            stock_number: "AX21-0061".into(),
            vin: "JTMRWRFV8LD072316".into(),
            year: 2021,
            make: "Toyota".into(),
            model: "RAV4".into(),
            trim_level: Some("TRD Off-Road".into()),
            condition: "cpo".into(),
            status: "available".into(),
            body_type: "suv".into(),
            mileage: 48680,
            list_price_cents: 2525000,
            msrp_cents: Some(2900000),
            exterior_color: Some("Lunar Rock".into()),
            drivetrain: Some("AWD".into()),
            fuel_type: Some("Regular gasoline".into()),
        }
    }

    fn lead() -> AdfLead {
        AdfLead {
            lead_id: "0199a1b2-c3d4-7e5f-8a90-1b2c3d4e5f60".into(),
            requested_at: DateTime::parse_from_rfc3339("2026-09-21T19:30:00Z")
                .unwrap()
                .with_timezone(&Utc),
            timezone: chrono_tz::America::Chicago,
            vehicles: vec![vehicle()],
            first_name: Some("María".into()),
            last_name: Some("Ñúñez".into()),
            email: Some("maria@example.com".into()),
            phone: Some("+12105550143".into()),
            comments: "Wants AWD. Test drive booked Tuesday September 22, 10:00 AM CDT.".into(),
            dealer_name: "Automotrix".into(),
        }
    }

    /// Run once with `cargo test -p automotrix write_golden -- --ignored` after an
    /// intentional format change, then review the diff by hand before committing.
    #[test]
    #[ignore]
    fn write_golden() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/golden/lead.xml");
        std::fs::write(path, render(&lead()).unwrap()).unwrap();
    }

    #[test]
    fn golden_document() {
        let xml = render(&lead()).unwrap();
        let expected = include_str!("../tests/golden/lead.xml");
        pretty_assertions::assert_eq!(xml, expected);
    }

    #[test]
    fn escapes_hostile_customer_text() {
        let mut l = lead();
        l.first_name = Some("Bobby <script>".into());
        l.last_name = Some("O'Brien & Sons \"Jr\"".into());
        l.comments = "Quiere un auto grande 🚙 — presupuesto <$30k & rápido".into();

        let xml = render(&l).unwrap();
        // raw markup must not survive
        assert!(!xml.contains("<script>"));
        assert!(xml.contains("&lt;script&gt;"));
        // quick-xml escapes the apostrophe and quotes too; that is valid XML,
        // and the parse-back below is what actually proves it
        assert!(xml.contains("O&apos;Brien &amp; Sons"));
        assert!(xml.contains("&quot;Jr&quot;"));
        // accents and emoji are legal UTF-8 text, they pass through unchanged
        assert!(xml.contains("🚙"));
        assert!(xml.contains("rápido"));

        // and it still parses
        let mut reader = quick_xml::Reader::from_str(&xml);
        loop {
            match reader.read_event() {
                Ok(Event::Eof) => break,
                Ok(_) => {}
                Err(e) => panic!("hostile text broke the XML: {e}"),
            }
        }
    }

    #[test]
    fn parses_back_with_every_required_node() {
        let xml = render(&lead()).unwrap();
        let mut reader = quick_xml::Reader::from_str(&xml);
        let mut seen: Vec<String> = Vec::new();
        loop {
            match reader.read_event() {
                Ok(Event::Start(e)) => {
                    seen.push(String::from_utf8_lossy(e.name().as_ref()).to_string())
                }
                Ok(Event::Eof) => break,
                Ok(_) => {}
                Err(e) => panic!("generated XML does not re-parse: {e}"),
            }
        }
        for required in [
            "adf", "prospect", "id", "requestdate", "vehicle", "year", "make", "model",
            "vin", "stock", "price", "customer", "contact", "name", "email", "phone",
            "comments", "vendor", "vendorname", "provider",
        ] {
            assert!(seen.iter().any(|n| n == required), "missing <{required}>");
        }
    }

    #[test]
    fn cpo_is_reported_as_used() {
        // ADF 1.0 has no certified status; calling a CPO car "new" would be a
        // misrepresentation in the CRM
        assert_eq!(adf_status("cpo"), "used");
        assert_eq!(adf_status("new"), "new");
        assert_eq!(adf_status("used"), "used");
    }

    #[test]
    fn declaration_and_processing_instruction_are_present() {
        let xml = render(&lead()).unwrap();
        assert!(xml.starts_with("<?xml version=\"1.0\"?>"));
        assert!(xml.contains("<?adf version=\"1.0\"?>"));
    }
}

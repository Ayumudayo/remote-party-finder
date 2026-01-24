use crate::ffxiv::Language;
use crate::listing::JobFlags;
use crate::listing::PartyFinderCategory;
use crate::listing_container::QueriedListing;
use crate::sestring_ext::SeStringExt;
use askama::Template;
use std::borrow::Borrow;

#[derive(Debug, Template)]
#[template(path = "listings.html")]
pub struct ListingsTemplate {
    pub containers: Vec<RenderableListing>,
    pub lang: Language,
}

#[derive(Debug)]
pub struct RenderableListing {
    pub container: QueriedListing,
    pub members: Vec<RenderableMember>,
    /// 파티장 로그 정보 (멤버 정보가 없어도 표시 가능)
    pub leader_parse_percentile: Option<u8>,
    pub leader_parse_color_class: String,
    pub leader_secondary_parse_percentile: Option<u8>,
    pub leader_secondary_parse_color_class: String,
    pub leader_has_secondary: bool,
}

/// 멤버 정보 + 해당 슬롯의 잡 ID
#[derive(Debug)]
pub struct RenderableMember {
    pub job_id: u8,
    pub player: crate::player::Player,
    pub parse_percentile: Option<u8>,
    pub parse_color_class: String,
    pub secondary_parse_percentile: Option<u8>,
    pub secondary_parse_color_class: String,
    pub has_secondary: bool,
}

impl RenderableMember {
    /// 잡 코드 반환 (예: "WHM", "PLD")
    pub fn job_code(&self) -> Option<&'static str> {
        crate::ffxiv::JOBS.get(&(self.job_id as u32)).map(|cj| cj.code())
    }
    
    /// 역할에 따른 CSS 클래스 반환 ("tank", "healer", "dps")
    pub fn role_class(&self) -> &'static str {
        use ffxiv_types::Role;
        if let Some(cj) = crate::ffxiv::JOBS.get(&(self.job_id as u32)) {
            match cj.role() {
                Some(Role::Tank) => "tank",
                Some(Role::Healer) => "healer",
                Some(Role::Dps) => "dps",
                None => "",
            }
        } else {
            ""
        }
    }
}

// Deref to QueriedListing to make template access compatible (e.g. methods)?
// Or just access .container in template.
// Actually, Deref might be easier for migration.
impl std::ops::Deref for RenderableListing {
    type Target = QueriedListing;
    fn deref(&self) -> &Self::Target {
        &self.container
    }
}

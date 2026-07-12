use super::{Steam, workshop::WorkshopPage};
use crate::Addon;

impl Steam {
    pub fn browse_subscribed_addons_page(&self, page: u32) -> Option<WorkshopPage> {
        self.browse_user_workshop_page(
            steamworks::UserList::Subscribed,
            steamworks::UserListOrder::SubscriptionDateDesc,
            page,
            None,
        )
    }
}

fn addon_page_from_workshop_page(page: WorkshopPage) -> (u32, Vec<Addon>) {
    (
        page.total_results,
        page.items.into_iter().map(Addon::from).collect(),
    )
}

pub fn browse_subscribed_addons_page(steam: &Steam, page: u32) -> Option<WorkshopPage> {
    steam.browse_subscribed_addons_page(page)
}

pub fn browse_subscribed_addons(steam: &Steam, page: u32) -> Option<(u32, Vec<Addon>)> {
    browse_subscribed_addons_page(steam, page).map(addon_page_from_workshop_page)
}

#[cfg(test)]
mod tests {
    use steamworks::PublishedFileId;

    use super::{WorkshopPage, addon_page_from_workshop_page};
    use crate::Addon;
    use crate::steam::workshop::WorkshopItem;

    #[test]
    fn subscribed_page_maps_to_addon_payload() {
        let page = WorkshopPage {
            total_results: 2,
            items: vec![WorkshopItem::from(PublishedFileId(123))],
        };

        let (total, addons) = addon_page_from_workshop_page(page);

        assert_eq!(total, 2);
        assert_eq!(addons.len(), 1);
        match &addons[0] {
            Addon::Workshop(item) => {
                assert_eq!(item.id, PublishedFileId(123));
                assert!(item.dead);
            }
            Addon::Installed(_) => panic!("expected Workshop addon payload"),
        }
    }
}

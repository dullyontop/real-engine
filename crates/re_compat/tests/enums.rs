//! The enum tables (docs/M2_PLAN.md, WP10): the data, the Luau objects and
//! the coercion of enum property writes. Every test is inside `mod enums`, so
//! `cargo test -p re_compat enums` selects them.

mod enums {
    use mlua::Lua;
    use re_compat::data::DataSet;
    use re_compat::{EnumItem, Status, datatypes};

    #[test]
    fn every_enum_is_usable_and_ordered() {
        let data = DataSet::builtin().unwrap();
        let mut count = 0;
        for (file, f) in &data.enums {
            for e in &f.enums {
                count += 1;
                assert_eq!(e.status, Status::Implemented, "{file}: Enum.{}", e.name);
                assert!(
                    e.items.windows(2).all(|w| w[0].value < w[1].value),
                    "{file}: Enum.{} is not in value order",
                    e.name
                );
                for i in &e.items {
                    let item = EnumItem::lookup(&e.name, &i.name).unwrap();
                    assert_eq!(
                        (item.enum_name(), item.name(), item.value()),
                        (e.name.as_str(), i.name.as_str(), i.value)
                    );
                    assert_eq!(EnumItem::from_value(&e.name, i.value), Some(item));
                }
            }
        }
        assert_eq!(count, re_compat::enums::enum_names().count());
    }

    #[test]
    fn enums_in_luau() {
        let lua = Lua::new();
        datatypes::install(&lua, &lua.globals(), 0).unwrap();
        lua.sandbox(true).unwrap();
        let src = include_str!("fixtures/datatypes/enums.luau");
        lua.load(src).set_name("@enums.luau").exec().unwrap();
        // GetEnumItems lists every item in value order.
        let names: Vec<String> = lua
            .load("local t = {} for _, i in Enum.RunContext:GetEnumItems() do table.insert(t, i.Name) end return t")
            .eval()
            .unwrap();
        assert_eq!(names, ["Legacy", "Server", "Client", "Plugin"]);
    }
}

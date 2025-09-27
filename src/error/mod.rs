#[macro_export]
macro_rules! function {
    () => {{
        const fn f() {}
        fn type_name_of<T>(_: T) -> &'static str {
            std::any::type_name::<T>()
        }
        let name = type_name_of(f);
        name.strip_suffix("::f").unwrap()
    }};
}

#[macro_export]
macro_rules! mod_err {
    ($($e:expr),*) => {
        anyhow!(
            "{} (in {} [{}:{}]) :: {}",
            function!(),
            module_path!(),
            file!(),
            line!(),
            anyhow!($($e),*)
        )
    };
}

#[macro_export]
macro_rules! concat_err {
    ($($err:expr),+) => {{
        let errors = vec![$(anyhow!($err).to_string()),+].join("\n");
        anyhow!(errors)
    }};
}

#[macro_export]
macro_rules! nest_errors {
    ($parent:expr, $($children:ident),+) => {{
        let errors = vec![anyhow!($parent).to_string(), $($children.to_string()),+].join("\n");
        anyhow!(
            "{} (in {} [{}:{}]) :: {}",
            function!(),
            module_path!(),
            file!(),
            line!(),
            errors

        )
    }};
}

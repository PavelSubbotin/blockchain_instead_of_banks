// примерная реализация смарт-контракта, заменяющего банковские операции на основе акторной модели

pub struct Compound {
    token_address: ActorId,   // id контракта
    ctoken_address: ActorId,  // id контракта, используемый для возвращения денег с процентами
    interest_rate: u128,      //  процент вклада
    collateral_factor: u128,  // сколько можно взять в процентах
    borrow_rate: u128,        // процент по кредиту
    ctoken_rate: u128,        // token cost * `ctoken_rate` = ctoken cost
    user_assets: BTreeMap<ActorId, Assets>, // таблица вкладов и кредитов с процентами для пользователей
    init_time: u64,           // время инициализации контракта
}

static mut COMPOUND_CONTRACT: Option<Compound> = None; // состояние контракта

impl Compound {               // инплементация контракта
    pub async fn lend_tokens(mut self, amount: u128) {
        asserts::greater_zero(amount, "Lend token amount"); // проверяем, что сумма положительна
        let msg_source = msg::source(); // адрес того, кто вызвал lend_tokens

        transfer_tokens( // переводим amount токенов с типом token_address с msg_source на адрес контракта (program_id)
            self.token_address,
            msg_source,
            exec::program_id(),
            amount,
        )
        .await;

        let ctokens_amount = Compound::count_ctokens(amount, self.ctoken_rate);

        transfer_tokens( // получаем обратно ctokenы
            self.ctoken_address,
            exec::program_id(),
            msg_source,
            ctokens_amount,
        )
        .await;

        self.user_assets        // обновляем информацию о количестве токенов пользователя в общей таблице
            .entry(msg_source)
            .and_modify(|assets| assets.add_lend(ctokens_amount, self.interest_rate))
            .or_insert_with(|| Assets::new(ctokens_amount, self.interest_rate));

        msg::reply(      // посылаем сообщение о том, что на определенный адрес было записано определенное количество ctokens
            CompoundEvent::TokensLended {
                address: msg_source,
                amount,
                ctokens_amount,
            },
            0,
        )
    }

    pub async fn borrow_tokens(mut self, amount: u128) {
        asserts::greater_zero(amount, "Borrow token amount"); // проверяем на положительность
        let msg_source = msg::source();

        let assets = self // проверяем, что пользователь вложил деньги (нужно для исбыточного обеспечения)
            .user_assets
            .get_mut(msg_source)
            .unwrap_or_else(|| panic!("No assets found for user = {:?}", msg_source));

        if Compound::count_tokens( // проверяем, что пользователь может занять запрошенное количество денег
            safe_mul(assets.lent_amount, self.collateral_factor),
            self.ctoken_rate,
        ) < assets.borrowed_amount + amount
        {
            panic!(
                "Not possible to borrow {} tokens due to the collateral factor",
                amount
            )
        }

        transfer_tokens(  // если проверки были успешны переводим пользователю токены
            self.token_address,
            exec::program_id(),
            msg_source,
            amount,
        )
        .await;

        self.user_assets  // обновляем информацию о количестве токенов пользователя в общей таблице
            .entry(msg_source)
            .and_modify(|assets| assets.add_borrow(amount, self.borrow_rate));

        msg::reply(       // // посылаем сообщение о том, что на определенный адрес было записано определенное количество токенов
            CompoundEvent::TokensBorrowed {
                address: msg_source,
                amount,
                borrow_rate: self.borrow_rate,
            },
            0,
        )
    }

    pub async fn refund_tokens(mut self, amount: u128) {  // функция возврата занятых средств
        asserts::greater_zero(amount, "Refund token amount"); // проверяем на положительность
        let msg_source = msg::source(); // получаем адрес инициатора

        let assets = self   // проверяем, что у пользователя есть счет и на нем достаточно токенов
            .user_assets
            .get_mut(msg::source())
            .unwrap_or_else(|| panic!("No assets found for user = {:?}", msg_source));
        assert!(
            assets.get_borrow_amount() >= amount,
            "Amount is bigger than possible"
        );

        transfer_tokens(    // если проверки прошли успешно переводим токены пользователя на адрес контракта
            self.token_address,
            msg_source,
            exec::program_id(),
            amount,
        )
        .await;

        self.user_assets.entry(msg_source).and_modify(|assets| { // обновляем информацию о балансе пользователя
            assets.borrowed_amount -= amount;
            assets.borrow_offset -= amount as i128;
        });

        msg::reply(           // посылаем инфу, что пользователь закрыл задолженность
            CompoundEvent::TokensRefunded {
                address: msg_source,
                amount,
            },
            0,
        )
    }

    pub async fn withdraw_tokens(mut self, amount: u128) { // функция вывода токенов
        let msg_source = msg::source(); // получаем адрес инициатора

        let assets = self // проверяем, что у пользователя есть баланс
            .user_assets
            .get_mut(msg_source)
            .unwrap_or_else(|| panic!("No assets found for user = {:?}", msg_source));

        assert!(          // проверяем, что на счете достточное количество токенов
            Compound::count_tokens(assets.get_lent_amount(), self.ctoken_rate) < amount,
            "Amount is bigger than possible"
        );

        if Compound::count_tokens(   // проверяем, что после вывода токенов не сломается концепция исбыточного обеспечения
            safe_mul(assets.get_lent_amount() - amount, self.collateral_factor),
            self.ctoken_rate,
        ) < assets.get_borrow_amount()
        {
            panic!(
                "Not possible to withdraw {} tokens due to the collateral factor",
                amount
            )
        }

        transfer_tokens(   // если все проверки пройдены, забираем ctokens
            self.ctoken_address,
            msg_source,
            exec::program_id(),
            Compound::count_ctokens(amount, self.ctoken_rate),
        )
        .await;

        transfer_tokens(   // взамен ctokens трансферим tokens
            self.token_address,
            exec::program_id(),
            msg_source,
            amount,
        )
        .await;

        self.user_assets.entry(msg_source).and_modify(|assets| { // обновляем информацию о балансе пользователя
            assets.lent_amount -= amount;
            assets.lend_offset -= amount as i128;
        });

        msg::reply(  // посылаем инфу об успешном выводе средств
            CompoundEvent::TokensWithdrawed {
                address: msg_source,
                amount,
            },
            0,
        )
        .expect("Error in reply");
    }



    fn count_ctokens(tokens_amount: u128, ctoken_rate: u128) -> u128 {
        safe_mul(tokens_amount, ctoken_rate)
    }

    fn count_tokens(ctokens_amount: u128, ctoken_rate: u128) -> u128 {
        safe_div(ctokens_amount, ctoken_rate)
    }
}

async unsafe fn main() {
    let action: CompoundAction = msg::load();
    let compound: mut Compound = unsafe { COMPOUND_CONTRACT.get_or_insert(Default::default()) };  // из сообщения получаем действие, которое нужно совершить

    match action {    // запускаем целевую функцию
        CompoundAction::LendTokens { amount } => compound.lend_tokens(amount).await,
        CompoundAction::BorrowTokens { amount } => compound.borrow_tokens(amount).await,
        CompoundAction::RefundTokens { amount } => compound.refund_tokens(amount).await,
        CompoundAction::WithdrawTokens { amount } => compound.withdraw_tokens(amount).await,
    }
}

pub unsafe fn init() {  //инициализация нового контракта
    let config: CompoundInit = msg::load().expect("Unable to decode CompoundInit");

    asserts::not_zero_address(&config.token_address, "Init token address");    // проверяем, что переданные данные корректны
    asserts::not_zero_address(&config.ctoken_address, "Init ctoken address");
    asserts::greater_zero(config.interest_rate, "Init interest rate");
    asserts::greater_zero(config.collateral_factor, "Init collateral factor");
    asserts::greater_zero(config.borrow_rate, "Init borrow rate");

    let compound = Compound {    
        token_address: config.token_address,
        ctoken_address: config.ctoken_address,
        init_time: exec::block_timestamp() / 1000,
        interest_rate: config.interest_rate,
        ctoken_rate: config.ctoken_rate,
        collateral_factor: config.collateral_factor,
        borrow_rate: config.borrow_rate,
        ..Default::default()
    };

    COMPOUND_CONTRACT = Some(compound);  //создаем контракт с переданными данными
}
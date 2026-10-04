//! 大量メッセージフィードの合成データジェネレーター。
//!
//! 指定件数のフィードを指定ユーザ数に割り当てて生成する。
//! 一定割合（デフォルト3%）を「コンプラ違反の種」として意図的に埋め込み、
//! 残りは通常のビジネスメッセージとする。
//!
//! 注意: ここで混入する比率はあくまで生成時の「種」の比率であり、
//! 実際にジャッジ（LLM）がNGと判定する比率とは一致しない場合がある。
//! 両者を区別してレポートすること。

use chrono::{DateTime, Duration, Utc};
use rand::RngExt;
use rand::seq::IndexedRandom;

use super::feed::{Feed, FeedId, MessageBody, TimeSentUtc, UserName};

/// 生成時に意図的に埋め込んだラベル。実際のLLM判定とは独立。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeedLabel {
    /// 通常のビジネスメッセージとして生成。
    Benign,
    /// コンプラ違反の種として意図的に生成。
    Risky,
}

impl SeedLabel {
    /// 「期待される判定」をOK/NG文字列として表す（Accuracy/Precision/Recall計算・
    /// レポート表示・SQLiteへの保存に使う）。
    #[must_use]
    pub fn expected_str(&self) -> &'static str {
        match self {
            SeedLabel::Benign => "OK",
            SeedLabel::Risky => "NG",
        }
    }
}

/// 生成されたフィードと、生成時に付与したラベルの組。
#[derive(Debug, Clone)]
pub struct SeededFeed {
    pub feed: Feed,
    pub seed_label: SeedLabel,
}

/// ジェネレーターの設定。
#[derive(Debug, Clone)]
pub struct GeneratorConfig {
    /// 生成するフィード総数。
    pub total_feeds: u64,
    /// 想定ユーザ数。
    pub user_count: u32,
    /// コンプラ違反の種として埋め込む割合（0.0〜1.0）。
    pub ng_seed_rate: f64,
}

impl Default for GeneratorConfig {
    fn default() -> Self {
        Self {
            total_feeds: 1_000,
            user_count: 5,
            ng_seed_rate: 0.03,
        }
    }
}

/// 生成結果のサマリ。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenerationReport {
    pub total_generated: u64,
    pub seeded_ng_count: u64,
}

impl GenerationReport {
    #[must_use]
    pub fn seeded_ng_rate(&self) -> f64 {
        if self.total_generated == 0 {
            return 0.0;
        }
        // 件数は数十万件規模（f64の52bit仮数部で正確に表現できる範囲）に収まる想定のため、
        // u64->f64変換による精度損失は実運用上問題にならない。
        #[allow(clippy::cast_precision_loss)]
        {
            self.seeded_ng_count as f64 / self.total_generated as f64
        }
    }
}

// 20カテゴリ × 10件 = 200件。LinkedIn風の通常ビジネス投稿を想定した多様な文面。
const BENIGN_TEMPLATES: &[&str] = &[
    // プロジェクト進捗
    "Sharing today's project progress update — things are moving along nicely.",
    "Quick status check: this sprint we closed out 18 of 20 planned tickets.",
    "Project Atlas just cleared its second milestone ahead of schedule.",
    "Wrapping up a productive week — the migration work is about 80% done.",
    "Our team shipped the new dashboard feature a day early.",
    "QA testing wrapped up this morning with zero blocking issues.",
    "The redesign project has moved into its final review phase.",
    "Happy to report the integration work finished ahead of deadline.",
    "This sprint's velocity was our best yet — great work, team.",
    "Rollout of the new onboarding flow went smoothly this morning.",
    // 新メンバー歓迎
    "Welcoming our newest team member! Excited to work together.",
    "Please join me in welcoming three new engineers to the platform team.",
    "So glad to have a new designer joining us this week.",
    "Our team just grew by two — welcome aboard!",
    "Thrilled to welcome our new VP of Operations starting Monday.",
    "A warm welcome to the newest member of our customer success team.",
    "Excited to introduce our new intern cohort starting this summer.",
    "Welcome to the team! Can't wait to see what we build together.",
    "Please help me welcome our new product manager to the group.",
    "New faces, new energy — welcoming three recruits to the sales team.",
    // ミーティング・議題共有
    "Sharing next week's meeting agenda. Please take a look.",
    "Quick reminder: the all-hands is moved to 10am tomorrow.",
    "Agenda for Thursday's planning session is now up in the shared doc.",
    "Looking forward to our quarterly sync next Tuesday — agenda attached.",
    "Reminder to RSVP for Friday's roadmap review meeting.",
    "Posting this week's stand-up notes for anyone who missed it.",
    "The leadership offsite agenda is finalized — check your inbox.",
    "Our retrospective is scheduled for 3pm, notes will follow after.",
    "Sharing the updated timeline we discussed in today's sync.",
    "Meeting notes from this morning's kickoff are now available.",
    // カンファレンス・イベント参加
    "Just got back from an industry conference — learned a lot.",
    "Great few days at the developer summit, so many good talks.",
    "Attended a fantastic workshop on design systems this week.",
    "Back from the trade show with some exciting new ideas.",
    "Had a wonderful time at the regional tech meetup last night.",
    "Just wrapped up three days at our annual company conference.",
    "Loved the keynote at this year's product leadership summit.",
    "Spent the day at a local hackathon — what a blast.",
    "Grateful for the chance to attend this year's industry expo.",
    "Came back from the conference with a notebook full of ideas.",
    // 目標達成・マイルストーン
    "We hit our quarterly goals! Huge thanks to the whole team.",
    "Celebrating 100,000 users on the platform today.",
    "Our support team just closed its 10,000th ticket this year.",
    "Proud to share we hit our annual revenue target early.",
    "Milestone unlocked: five years in business as of today.",
    "We just crossed one million downloads — thank you all.",
    "Celebrating our best quarter yet for customer retention.",
    "Hit a huge milestone today: our 50th enterprise customer.",
    "Thrilled to report we beat our Q3 targets across the board.",
    "Today marks two years since we launched this product.",
    // 新製品発表
    "Announcing the release of our new product.",
    "Excited to launch our redesigned mobile app today.",
    "Our new analytics suite is live as of this morning.",
    "Introducing the newest addition to our product lineup.",
    "We just shipped a major update to our core platform.",
    "Today we're rolling out dark mode to all users.",
    "Proud to unveil our new customer portal.",
    "Launch day! Our new API is now publicly available.",
    "We're excited to announce our newest integration partner.",
    "The beta program for our new feature opens today.",
    // キャリアの振り返り
    "Had a great chance to reflect on my career path lately.",
    "Thinking back on the past five years at this company — what a ride.",
    "Reflecting on everything I've learned since switching careers.",
    "A year ago today I started this job, and I'm grateful for the journey.",
    "Looking back at my first year in tech leadership.",
    "Some thoughts on growth after a decade in this industry.",
    "Taking a moment to appreciate how far our team has come.",
    "Reflecting on the mentors who shaped my early career.",
    "Ten years in, and I'm still learning something new every day.",
    "A short reflection on what this promotion means to me.",
    // 書籍・記事の推薦
    "The business book I read recently was really insightful.",
    "Just finished a great read on leadership and wanted to share it.",
    "This article on team culture is worth your ten minutes.",
    "Picked up a book on negotiation that changed how I think about deals.",
    "Highly recommend this podcast episode on product strategy.",
    "Finished a great book on habits — lots of practical takeaways.",
    "Sharing a short read that reframed how I think about feedback.",
    "This newsletter issue on market trends was particularly sharp.",
    "A quick book recommendation for anyone leading remote teams.",
    "Just wrapped up a book club pick on decision-making.",
    // リモートワーク・生産性
    "Had a good discussion today on making remote work more efficient.",
    "Sharing a few tips that have helped our distributed team stay aligned.",
    "Our async-first workflow has cut meeting time in half this quarter.",
    "Three small changes that made our hybrid schedule work better.",
    "A quick thread on how we structure focus time across time zones.",
    "Our team's new no-meeting Wednesdays have been a game changer.",
    "Sharing how we keep remote onboarding personal and effective.",
    "A few lessons learned from two years of fully remote work.",
    "How we redesigned our stand-ups for a distributed team.",
    "Tips for staying connected with teammates across time zones.",
    // 顧客からのフィードバック
    "Sharing some wonderful feedback we received from a customer.",
    "A customer shared this story with us and we had to post it.",
    "Nothing beats hearing directly from happy customers like this.",
    "This review made our whole team's day.",
    "Grateful for the kind words from one of our longtime clients.",
    "Sharing a testimonial that really captures what we're building toward.",
    "A customer took the time to write this, and it means a lot.",
    "This is exactly why we do what we do — thank you for sharing.",
    "Proud to share this glowing review from a new customer.",
    "Love seeing feedback like this land in our inbox.",
    // 採用告知
    "We're hiring! Check out our open roles on the careers page.",
    "Excited to announce we're expanding the engineering team this quarter.",
    "Our design team is growing — open roles linked below.",
    "Looking for a product manager to join our growing team.",
    "We just opened three new roles on the data team.",
    "Hiring alert: we're looking for a customer success lead.",
    "Our team is expanding into a new market — join us.",
    "We're on the hunt for a talented backend engineer.",
    "New openings just posted for our support and success teams.",
    "Growing fast and hiring across every department this year.",
    // 感謝の投稿
    "Huge thanks to everyone who made this launch possible.",
    "Grateful for such a supportive team this year.",
    "Thank you to our partners for making this milestone possible.",
    "A heartfelt thank you to the team that pulled off this release.",
    "So thankful for the mentors who guided me this year.",
    "Appreciation post: this team never stops impressing me.",
    "Thank you to our customers for trusting us with their business.",
    "Grateful for the support during a really busy quarter.",
    "Shoutout to the ops team for keeping everything running smoothly.",
    "Thank you to everyone who joined us at the event last week.",
    // 業界トレンド・意見
    "Some thoughts on where our industry is heading this year.",
    "An interesting shift I've noticed in how customers evaluate vendors lately.",
    "Sharing a few predictions for where this market goes next.",
    "The pace of change in this space keeps surprising me.",
    "A quick take on the latest trends shaping our industry.",
    "Noticing an interesting pattern in how teams are adopting automation.",
    "My two cents on the direction our industry is moving.",
    "A short opinion piece on what's next for this sector.",
    "Thinking out loud about where the next big opportunity lies.",
    "Some observations after a decade watching this market evolve.",
    // ボランティア・慈善活動
    "Our team spent the day volunteering at the local food bank.",
    "Proud to support this year's charity fundraiser with the whole office.",
    "We donated a portion of this quarter's profits to local schools.",
    "Great turnout for our annual community cleanup day.",
    "Our volunteer program hit a new milestone this year.",
    "Spent the afternoon mentoring students through our community outreach program.",
    "Thankful for the chance to give back this holiday season.",
    "Our team raised funds for a great cause this month.",
    "Volunteering with this organization has been one of the highlights of my year.",
    "Proud of our team for showing up to support the local shelter.",
    // 勤続記念
    "Celebrating five years with this amazing team today.",
    "Can't believe it's been three years since I joined this company.",
    "Happy work anniversary to one of our longest-tenured engineers.",
    "Ten years ago today I started my first day here.",
    "Marking two years on this team — what a journey.",
    "Congratulations to our colleague on reaching their tenth anniversary here.",
    "One year in, and I'm more excited about this company than ever.",
    "Celebrating a decade of building great products together.",
    "Happy anniversary to the team that's been with us since day one.",
    "Reflecting on four great years working alongside this team.",
    // メンターシップ
    "Grateful for the chance to mentor a few junior engineers this year.",
    "My mentor's advice from years ago still guides me today.",
    "Spent the afternoon mentoring students in our new graduate program.",
    "Proud to see my mentee land their first engineering role.",
    "A shoutout to everyone who has mentored me along the way.",
    "Started a new mentorship program for early-career hires this month.",
    "One of the best parts of this job is mentoring new teammates.",
    "Thankful for a manager who always made time to mentor the team.",
    "Our mentorship circle just welcomed its newest cohort.",
    "Sharing some thoughts on what makes a great mentor.",
    // 講演・ウェビナー
    "Had a great time speaking at this year's product conference.",
    "Excited to share I'll be speaking at next month's summit.",
    "Just wrapped up a panel discussion on engineering culture.",
    "Thanks to everyone who joined my talk on scaling teams.",
    "Honored to be invited to speak at the regional meetup.",
    "Gave a workshop today on effective cross-team collaboration.",
    "Looking forward to presenting at the upcoming industry roundtable.",
    "Just finished recording a webinar on product strategy.",
    "Thanks for the great questions after today's fireside chat.",
    "Proud to have represented our team on today's expert panel.",
    // 受賞・表彰
    "Thrilled to share our team won an industry award this week.",
    "Proud to be named one of this year's top workplaces.",
    "Our product was just recognized with a design excellence award.",
    "Congratulations to our colleague for winning employee of the year.",
    "Honored to receive this recognition from our industry association.",
    "Our customer support team was ranked number one in the region.",
    "So proud of the team for this well-deserved recognition.",
    "Grateful to be included on this year's innovation list.",
    "Celebrating our CEO's recognition as a top industry leader.",
    "Another award for the books — proud of this team.",
    // 業務改善
    "Our new release process cut deployment time by half.",
    "Small process tweak, big impact: fewer meetings, more focus time.",
    "We streamlined our support workflow and response times are way down.",
    "A simple change to our code review process made a big difference.",
    "Our new ticket triage system is already paying off.",
    "We cut our onboarding time from six weeks to two.",
    "Sharing how a small workflow change improved our sprint planning.",
    "Our new documentation process has made handoffs so much smoother.",
    "A quick win: automating our weekly reporting saved the team hours.",
    "Process nerds unite — here's how we fixed our bottleneck.",
    // 新ツール導入
    "Rolled out a new analytics tool across the team this week.",
    "Our engineering team just adopted a new testing framework.",
    "Excited to start using our new design system company-wide.",
    "We migrated to a new CRM and the switch has been smooth.",
    "Our support team is now using a new ticketing platform.",
    "Just adopted a new project management tool and loving it so far.",
    "Switching to this new deployment pipeline has sped things up a lot.",
    "Our data team rolled out a new dashboarding tool today.",
    "We just integrated a new scheduling tool into our workflow.",
    "Excited to announce we've adopted a new collaboration platform team-wide.",
];

// 20カテゴリ × 10件 = 200件。企業コンプライアンス違反の典型例を想定した多様な文面。
const RISKY_TEMPLATES: &[&str] = &[
    // インサイダー情報・財務情報の漏洩
    "Sharing unpublished earnings info right here — buy before the stock price jumps.",
    "Insider tip: the acquisition is already agreed, buy now before it's announced.",
    "Our Q3 numbers aren't public yet, but between us, they're huge — load up on shares.",
    "Heads up, we're getting bought out next month, this isn't public yet.",
    "Don't tell anyone, but the board just approved numbers way above street estimates.",
    "The merger is basically signed, just waiting on the press release — act fast.",
    "I saw the unreleased financials and they're bad, might want to sell before it drops.",
    "Between us, the CFO confirmed the buyout price — way above market.",
    "Our biggest client is about to be announced tomorrow, stock will pop, just saying.",
    "The layoffs announcement isn't public yet, but it's coming and the stock will tank.",
    // 個人情報（PII）の漏洩
    "Sharing a list of customer personal info (names, phone numbers, addresses).",
    "Here's the full customer export with emails and billing details attached.",
    "Posting the spreadsheet with everyone's home addresses for the holiday party planning.",
    "Attaching the customer database dump here for anyone who needs it.",
    "Here are the SSNs we collected during onboarding, just in case anyone needs them.",
    "Sharing the full list of applicant resumes including phone numbers and addresses.",
    "Pasting the customer contact list here instead of using the secure portal.",
    "Here's everyone's date of birth from the HR system for the birthday calendar.",
    "Posting the unmasked payment details from last week's transactions.",
    "Sharing the full passport scan folder for the visa applicants here.",
    // 認証情報の漏洩
    "Posting the admin password and credentials right here.",
    "Here's the root password for the production database, just in case.",
    "Sharing my API key here so the team can use it directly.",
    "Pasting the shared login for the finance system in case anyone needs it.",
    "Here's the master password for all our client accounts.",
    "Just dropping the production SSH key here for convenience.",
    "Sharing the service account credentials instead of using the vault.",
    "Here's the admin token for the internal dashboard, don't lose it.",
    "Posting the Wi-Fi password and VPN credentials for the whole office.",
    "Attaching the database connection string with the password included.",
    // ハラスメント・いじめ
    "That new hire is useless, someone should just tell them to quit.",
    "Honestly this person doesn't deserve to be on this team, it's embarrassing.",
    "Can we talk about how annoying it is working with them every day?",
    "I'm done being nice, this person needs to be called out publicly.",
    "Everyone agrees this teammate is dragging the whole project down, naming them here.",
    "Posting this because I think everyone should know how difficult this person is.",
    "This person's work is a joke and they should be ashamed of it.",
    "Calling this out directly: stop sending me messages after hours, it's harassment.",
    "I'm tired of covering for this coworker, they need to be exposed.",
    "This person keeps making inappropriate comments and nobody is doing anything about it.",
    // 差別的な採用・言動
    "Sharing our unwritten internal rule not to hire people of this ethnicity.",
    "Honestly we should stop interviewing candidates over a certain age, they don't fit.",
    "Management quietly told us to avoid hiring anyone with an accent for client calls.",
    "We've been told not to promote women into this specific leadership track.",
    "There's an unofficial rule here about not hiring candidates from that country.",
    "We only consider candidates from certain universities, the rest go straight to reject.",
    "Managers have been told to avoid hiring anyone who might need parental leave soon.",
    "There's a quiet policy here to filter out candidates based on their religion.",
    "We were told to deprioritize applicants with disabilities during the screening round.",
    "Leadership mentioned preferring candidates who don't have young children, unofficially of course.",
    // 機密契約・顧客情報の漏洩
    "Pasting a client's confidential contract terms here. Take a look.",
    "Here's the pricing we gave our biggest client, way below list price.",
    "Sharing the NDA details from our partner negotiation, just for reference.",
    "Attaching the full vendor contract including the penalty clauses.",
    "Posting the client's confidential roadmap that they shared with us in private.",
    "Here's the exact discount structure we offered this client, don't tell sales.",
    "Sharing the terms of our exclusive deal with this partner, confidentially of course.",
    "Pasting the client's internal financials that they shared under NDA.",
    "Here's the settlement agreement details from last year's dispute.",
    "Attaching the unredacted licensing agreement with our key supplier.",
    // 違法に入手した競合情報
    "Sharing a competitor's confidential pricing list that we obtained illegally.",
    "Got this leaked roadmap from a contact at our competitor, take a look.",
    "Someone sent me our rival's internal strategy deck, sharing it here.",
    "This competitor's unreleased product specs landed in my inbox somehow.",
    "Here's our competitor's customer list, got it from a former employee.",
    "Sharing a competitor's internal sales playbook someone passed along.",
    "Got hold of their investor deck before it was public, here it is.",
    "This is a competitor's confidential board presentation, interesting read.",
    "Someone forwarded me their rival's unreleased marketing plan, sharing for fun.",
    "Here's a leaked internal memo from a competing firm.",
    // 名誉毀損・同僚の公開批判
    "Naming names: this manager has been mistreating their reports for months.",
    "I'm calling out this colleague publicly for taking credit for my work.",
    "Let's be honest about who's actually responsible for this failure, tagging them here.",
    "This person has been lying about their credentials this whole time.",
    "Publicly noting that this teammate has been sabotaging the project on purpose.",
    "I think everyone should know this coworker got their last job through nepotism.",
    "Exposing this manager's favoritism toward certain team members, names included.",
    "This colleague has been spreading rumors about me and I'm done staying quiet.",
    "Calling out this vendor rep by name for lying to our team repeatedly.",
    "Naming the exact people who approved this disastrous decision.",
    // 安全インシデントの隠蔽
    "The injury on the factory floor last week wasn't reported, let's keep it that way.",
    "We're not telling regulators about the near-miss incident from Tuesday.",
    "Management asked us to keep the chemical spill off the official log.",
    "Don't mention the equipment failure in the safety report, it'll cause problems.",
    "We quietly fixed the fire exit issue without filing an incident report.",
    "The forklift accident last month was never logged, best to keep it that way.",
    "We're keeping the contaminated batch quiet instead of issuing a recall.",
    "Someone got hurt on site but we're handling it off the books.",
    "We're not disclosing the gas leak incident to the safety board.",
    "The malfunction that almost caused an accident is staying out of the report.",
    // 贈賄・リベート
    "The vendor offered us a personal kickback to push their contract through.",
    "We've been accepting gifts from this supplier in exchange for favorable terms.",
    "This client paid an under-the-table fee to speed up their approval.",
    "Our procurement lead is taking commissions from vendors on the side.",
    "We agreed to approve this deal in exchange for a personal favor.",
    "The contractor slipped us cash to skip the standard bidding process.",
    "We're routing this contract to a vendor who's paying us personally.",
    "This official accepted a bribe to fast-track our permit application.",
    "Our sales rep offered a kickback to the client's purchasing manager.",
    "We quietly accepted a personal payment to overlook the quality issues.",
    // 企業秘密の漏洩
    "Here's our proprietary algorithm, sharing it since someone outside asked nicely.",
    "Posting the formula we use internally, even though it's supposed to be secret.",
    "Sharing our confidential manufacturing process with a contact at another company.",
    "Here's the source code for our core product, thought it might help someone.",
    "Attaching our internal R&D notes that aren't meant to leave the building.",
    "Sharing the recipe our brand is built on, just between us.",
    "Here's our unreleased patent application details, interesting stuff.",
    "Posting our internal pricing algorithm that we've kept secret for years.",
    "Sharing the blueprint for our next-gen product ahead of the patent filing.",
    "Here's the trade secret process our competitors have been trying to figure out.",
    // 規制違反の告白
    "We've been operating without the required license for the past year.",
    "Honestly we never filed the environmental disclosure we were supposed to.",
    "We've been skipping the mandatory audits for cost reasons.",
    "Our emissions numbers have been off for months and we haven't reported it.",
    "We're not actually compliant with the new data protection rules yet.",
    "We've been ignoring the inspection notices from the regulator.",
    "Our factory hasn't met code in years, we just haven't been caught.",
    "We quietly stopped following the disclosure requirements last quarter.",
    "We've never actually registered this product with the regulator.",
    "Our compliance filings have been backdated to look current.",
    // 不正・詐欺の告白
    "We've been inflating our usage numbers in reports to investors.",
    "Honestly those sales figures were padded to hit the quarterly target.",
    "We billed this client for hours nobody actually worked.",
    "Someone altered the expense reports before they went to finance.",
    "We've been double-billing a few accounts and hoping nobody notices.",
    "Those testimonials on our site are made up, we never had real customers say that.",
    "We backdated this contract to make the numbers look better for the quarter.",
    "The audit numbers were adjusted before the external review.",
    "We've been reporting phantom accounts to inflate our growth metrics.",
    "Someone forged the signature on this approval document.",
    // 顧客データの不正利用
    "We've been selling customer browsing data without telling anyone.",
    "Quietly using customer location data for things outside the privacy policy.",
    "We shared the customer list with a third party without consent.",
    "Using customer support chat logs for ad targeting without disclosure.",
    "We've been tracking users beyond what the privacy policy allows.",
    "Selling anonymized data that's actually easy to re-identify, but nobody checks.",
    "We're using customer payment history for purposes they never agreed to.",
    "Quietly merging customer data sets that were supposed to stay separate.",
    "We've been using children's account data in ways the policy doesn't cover.",
    "Sharing customer health information with our marketing team, against policy.",
    // 価格カルテル・談合
    "We agreed with our competitor to keep prices above a certain floor.",
    "Had a call with the rival firm to align on next quarter's pricing.",
    "We're coordinating discount limits with the other vendors in this space.",
    "Our pricing team talked to a competitor about not undercutting each other.",
    "We agreed not to compete for this client's business this cycle.",
    "Had an informal chat with our rival about keeping rates steady industry-wide.",
    "We're syncing with competitors on surcharge timing so nobody looks worse.",
    "Coordinated with another firm to split this market instead of competing.",
    "We agreed to raise prices together next month, keep this quiet.",
    "Our team talked to a competitor about matching service fees exactly.",
    // 脱税スキーム
    "We're routing this revenue through an offshore account to avoid taxes.",
    "Keeping these earnings off the books so we don't report them.",
    "We're classifying these payments as something else to dodge the tax.",
    "Using a shell company to move this income around the tax filing.",
    "We've been underreporting revenue to reduce our tax bill for years.",
    "Structuring these payments to employees so they don't show up as income.",
    "We're hiding a portion of overseas earnings from the tax authority.",
    "Running personal expenses through the company to avoid personal tax.",
    "We've been misclassifying contractors to avoid payroll tax obligations.",
    "Moving this quarter's profit to a subsidiary just to lower our tax rate illegally.",
    // 報復の脅し
    "If you report this to HR, don't expect your contract to be renewed.",
    "Keep this complaint to yourself or your project assignments might change.",
    "If this gets escalated, I'll make sure your review reflects it.",
    "Don't file that report, it won't go well for your career here.",
    "If you talk to compliance about this, I'll remember it at bonus time.",
    "Report this and see what happens to your next promotion cycle.",
    "If this leaves the room, I'll make sure you regret bringing it up.",
    "Push this complaint further and you can forget about that transfer.",
    "Say anything to legal about this and your team gets reassigned.",
    "If you go over my head on this, there will be consequences.",
    // 記録の改ざん
    "We backdated this approval so it looks like it happened on time.",
    "Someone changed the timestamps on these safety logs after the fact.",
    "We edited the meeting minutes to remove the part about the complaint.",
    "The inspection report was altered before it went to the regulator.",
    "We forged the sign-off on this document since the manager was out.",
    "Someone changed the numbers in this audit trail after it was submitted.",
    "We quietly updated the incident log to make it look less serious.",
    "The training records were backfilled even though the sessions never happened.",
    "We altered the shipment records to hide the delay from the client.",
    "Someone edited the quality control sheet after the batch already shipped.",
    // 知的財産の盗用
    "I took the source code from my last job and we're using it here.",
    "We copied our competitor's design almost exactly for this new feature.",
    "This diagram is lifted straight from a patent that isn't ours.",
    "We used a former employer's client list to build our outreach campaign.",
    "This algorithm was built using code a teammate brought from their old job.",
    "We reverse-engineered their product and copied the core logic directly.",
    "I still have access to my old company's repo and pulled some code from it.",
    "We copied this UI almost pixel-for-pixel from a competitor's app.",
    "This document template was taken directly from my previous employer's files.",
    "We used proprietary research from another company without permission.",
    // 不正アクセス・晒し行為
    "I still have login access to my old team's systems and poked around.",
    "Logged into a coworker's account to check something without asking them.",
    "Found a way into the HR system and looked up some salary info out of curiosity.",
    "Someone's personal home address got posted here after a disagreement online.",
    "I used an old admin account that should've been disabled to peek at records.",
    "Shared someone's personal phone number here after they left the company on bad terms.",
    "Accessed the exec email account using credentials that were never revoked.",
    "Posted a former coworker's new employer and address after a dispute.",
    "Quietly downloaded the entire customer database before resigning, just in case.",
    "Posted a teammate's personal home address after a heated argument in the group chat.",
];

/// 設定に基づいてフィードを生成する。
///
/// ユーザは `user_count` 人想定で `user_{n}` 形式の名前をラウンドロビン的にランダム割当する。
/// `ng_seed_rate` の割合でリスキーテンプレートから本文を生成し、残りはベニンテンプレートから生成する。
///
/// # Panics
/// 生成した値が `UserName` / `MessageBody` のバリデーション（空文字・最大長）を
/// 満たさない場合にパニックする。テンプレートは本モジュール内で非空・長さ制限内に
/// 収まるよう固定されているため、通常の実行では発生しない。
#[must_use]
pub fn generate_feeds(config: &GeneratorConfig) -> (Vec<SeededFeed>, GenerationReport) {
    let mut rng = rand::rng();
    let user_count = config.user_count.max(1);
    let base_time: DateTime<Utc> = Utc::now() - Duration::hours(24);

    // total_feeds はCLIで指定される件数（想定上限は数十万件）であり、
    // 64bitターゲットでは usize への変換で切り捨ては発生しない。
    #[allow(clippy::cast_possible_truncation)]
    let mut feeds = Vec::with_capacity(config.total_feeds as usize);
    let mut seeded_ng_count: u64 = 0;

    for i in 0..config.total_feeds {
        let user_idx = rng.random_range(0..user_count);
        let user_name =
            UserName::new(format!("user_{user_idx:04}")).expect("generated user name is non-empty");

        let is_risky = rng.random_bool(config.ng_seed_rate.clamp(0.0, 1.0));
        let (label, template_pool) = if is_risky {
            (SeedLabel::Risky, RISKY_TEMPLATES)
        } else {
            (SeedLabel::Benign, BENIGN_TEMPLATES)
        };
        if is_risky {
            seeded_ng_count += 1;
        }

        let template = template_pool
            .choose(&mut rng)
            .expect("template pool is non-empty");
        let message = MessageBody::new(*template).expect("templates are valid non-empty messages");

        // 時系列は24時間の範囲内でランダムに前後させつつ、概ね送信順になるように緩やかに増加させる。
        let jitter_secs = rng.random_range(0..86_400i64);
        let time_sent = TimeSentUtc::new(base_time + Duration::seconds(jitter_secs));

        feeds.push(SeededFeed {
            feed: Feed::new(FeedId::new(i), user_name, message, time_sent),
            seed_label: label,
        });
    }

    let report = GenerationReport {
        total_generated: config.total_feeds,
        seeded_ng_count,
    };

    (feeds, report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seed_label_expected_str_maps_correctly() {
        assert_eq!(SeedLabel::Benign.expected_str(), "OK");
        assert_eq!(SeedLabel::Risky.expected_str(), "NG");
    }

    #[test]
    fn generates_requested_total_count() {
        let config = GeneratorConfig {
            total_feeds: 500,
            user_count: 5,
            ng_seed_rate: 0.03,
        };
        let (feeds, report) = generate_feeds(&config);
        assert_eq!(feeds.len(), 500);
        assert_eq!(report.total_generated, 500);
    }

    #[test]
    fn seeded_ng_rate_is_approximately_configured_rate() {
        let config = GeneratorConfig {
            total_feeds: 50_000,
            user_count: 100,
            ng_seed_rate: 0.03,
        };
        let (_, report) = generate_feeds(&config);
        let rate = report.seeded_ng_rate();
        // 統計的なばらつきを許容し、1%〜5%の範囲に収まることを確認する。
        assert!(
            rate > 0.01 && rate < 0.05,
            "expected seeded NG rate near 3%, got {rate}"
        );
    }

    #[test]
    fn user_names_are_within_configured_user_count() {
        let config = GeneratorConfig {
            total_feeds: 200,
            user_count: 5,
            ng_seed_rate: 0.03,
        };
        let (feeds, _) = generate_feeds(&config);
        let unique_users: std::collections::HashSet<_> = feeds
            .iter()
            .map(|f| f.feed.user_name.as_str().to_string())
            .collect();
        assert!(unique_users.len() <= 5);
    }

    #[test]
    fn zero_total_feeds_produces_empty_result() {
        let config = GeneratorConfig {
            total_feeds: 0,
            user_count: 5,
            ng_seed_rate: 0.03,
        };
        let (feeds, report) = generate_feeds(&config);
        assert!(feeds.is_empty());
        // 0件ならレートは常にちょうど0.0になる（除算を行わない早期returnのため）。
        #[allow(clippy::float_cmp)]
        {
            assert_eq!(report.seeded_ng_rate(), 0.0);
        }
    }

    #[test]
    fn feed_ids_are_unique_and_sequential() {
        let config = GeneratorConfig {
            total_feeds: 10,
            user_count: 2,
            ng_seed_rate: 0.5,
        };
        let (feeds, _) = generate_feeds(&config);
        let ids: Vec<u64> = feeds.iter().map(|f| f.feed.id.value()).collect();
        assert_eq!(ids, (0..10).collect::<Vec<_>>());
    }
}

// Captured by fixtures/companion/cases.py from the original pytest cases.
#[cfg(test)]
mod cases {
    include!("companion_cases.rs");
    #[test]
    fn test_a_click_on_an_answer_keeps_it_or_types_it_into_claude() {
        check("test_a_click_on_an_answer_keeps_it_or_types_it_into_claude");
    }
    #[test]
    fn test_a_dropped_item_is_lost_one_that_persists_is_open_and_bookkeeping_is_skipped() {
        check("test_a_dropped_item_is_lost_one_that_persists_is_open_and_bookkeeping_is_skipped");
    }
    #[test]
    fn test_a_transcript_gives_its_summaries_and_the_cli_prints_the_threads() {
        check("test_a_transcript_gives_its_summaries_and_the_cli_prints_the_threads");
    }
    #[test]
    fn test_an_empty_or_broken_summary_line_is_not_the_newest_summary() {
        check("test_an_empty_or_broken_summary_line_is_not_the_newest_summary");
    }
    #[test]
    fn test_drop_is_for_good_and_later_lasts_until_the_next_compaction() {
        check("test_drop_is_for_good_and_later_lasts_until_the_next_compaction");
    }
    #[test]
    fn test_the_answers_fit_a_forty_column_pane() {
        check("test_the_answers_fit_a_forty_column_pane");
    }
    #[test]
    fn test_the_section_shows_three_rows_with_answers_and_only_with_something_in_it() {
        check("test_the_section_shows_three_rows_with_answers_and_only_with_something_in_it");
    }
    #[test]
    fn test_waiting_on_you_is_flagged_and_ranks_first() {
        check("test_waiting_on_you_is_flagged_and_ranks_first");
    }
    #[test]
    fn test_a_go_never_answers_a_thread_that_asked_you_nothing() {
        check("test_a_go_never_answers_a_thread_that_asked_you_nothing");
    }
    #[test]
    fn test_a_thread_seen_once_that_never_waited_on_you_faded() {
        check("test_a_thread_seen_once_that_never_waited_on_you_faded");
    }
    #[test]
    fn test_a_thread_set_aside_for_later_is_marked_for_the_reminders() {
        check("test_a_thread_set_aside_for_later_is_marked_for_the_reminders");
    }
    #[test]
    fn test_a_thread_you_said_go_to_right_after_is_answered_not_lost() {
        check("test_a_thread_you_said_go_to_right_after_is_answered_not_lost");
    }
    #[test]
    fn test_every_thread_is_on_the_f_page_with_why_including_the_guesses() {
        check("test_every_thread_is_on_the_f_page_with_why_including_the_guesses");
    }
    #[test]
    fn test_one_thread_in_two_wordings_is_one() {
        check("test_one_thread_in_two_wordings_is_one");
    }
    #[test]
    fn test_prep_compacts_reminders_never_count_as_owed() {
        check("test_prep_compacts_reminders_never_count_as_owed");
    }
    #[test]
    fn test_the_f_page_scrolls_and_its_rows_open_with_their_answers() {
        check("test_the_f_page_scrolls_and_its_rows_open_with_their_answers");
    }
    #[test]
    fn test_a_side_task_handoff_says_so_and_a_handoff_is_no_reply_to_another_thread() {
        check("test_a_side_task_handoff_says_so_and_a_handoff_is_no_reply_to_another_thread");
    }
    #[test]
    fn test_a_thread_carried_from_the_session_before_is_handed_too() {
        check("test_a_thread_carried_from_the_session_before_is_handed_too");
    }
    #[test]
    fn test_only_a_sent_handoff_whose_quote_names_the_thread_counts() {
        check("test_only_a_sent_handoff_whose_quote_names_the_thread_counts");
    }
    #[test]
    fn test_the_sent_subagent_handoff_hides_its_thread_and_the_f_page_lists_it_handed() {
        check("test_the_sent_subagent_handoff_hides_its_thread_and_the_f_page_lists_it_handed");
    }
    #[test]
    fn test_while_the_ask_sits_in_claudes_prompt_drop_and_later_are_still_a_click_away() {
        check("test_while_the_ask_sits_in_claudes_prompt_drop_and_later_are_still_a_click_away");
    }
    #[test]
    fn test_a_carried_thread_this_session_already_carries_shows_once() {
        check("test_a_carried_thread_this_session_already_carries_shows_once");
    }
    #[test]
    fn test_a_finished_session_with_nothing_asked_ranks_its_tail_low_but_keeps_it() {
        check("test_a_finished_session_with_nothing_asked_ranks_its_tail_low_but_keeps_it");
    }
    #[test]
    fn test_a_prompt_claude_never_finished_answering_is_carried_first() {
        check("test_a_prompt_claude_never_finished_answering_is_carried_first");
    }
    #[test]
    fn test_a_thread_handed_on_before_the_restart_is_not_carried_nor_is_its_handoff() {
        check("test_a_thread_handed_on_before_the_restart_is_not_carried_nor_is_its_handoff");
    }
    #[test]
    fn test_claudes_last_question_is_carried_when_the_turn_ended() {
        check("test_claudes_last_question_is_carried_when_the_turn_ended");
    }
    #[test]
    fn test_the_tail_threads_key_is_a_sha256_of_its_session() {
        check("test_the_tail_threads_key_is_a_sha256_of_its_session");
    }
    #[test]
    fn test_a_drop_before_the_restart_holds_when_a_summary_rewords_the_thread() {
        check("test_a_drop_before_the_restart_holds_when_a_summary_rewords_the_thread");
    }
    #[test]
    fn test_a_go_names_a_thread_by_its_subject_not_its_verb() {
        check("test_a_go_names_a_thread_by_its_subject_not_its_verb");
    }
    #[test]
    fn test_a_second_restart_keeps_what_the_first_carried() {
        check("test_a_second_restart_keeps_what_the_first_carried");
    }
    #[test]
    fn test_lost_found_never_writes_into_the_carried_list() {
        check("test_lost_found_never_writes_into_the_carried_list");
    }
    #[test]
    fn test_the_tail_is_only_what_came_after_the_last_summary() {
        check("test_the_tail_is_only_what_came_after_the_last_summary");
    }
    #[test]
    fn test_a_lookup_after_six_other_calls_is_not_about_the_prompt() {
        check("test_a_lookup_after_six_other_calls_is_not_about_the_prompt");
    }
    #[test]
    fn test_a_word_of_your_prompt_looked_up_in_memory_within_six_calls_counts() {
        check("test_a_word_of_your_prompt_looked_up_in_memory_within_six_calls_counts");
    }
    #[test]
    fn test_two_sessions_not_in_claude_md_give_one_note_and_it_is_cached_by_mtime() {
        check("test_two_sessions_not_in_claude_md_give_one_note_and_it_is_cached_by_mtime");
    }
    #[test]
    fn test_a_pasted_block_is_not_your_words() {
        check("test_a_pasted_block_is_not_your_words");
    }
    #[test]
    fn test_harness_side_agent_and_dreamer_text_never_count_as_yours() {
        check("test_harness_side_agent_and_dreamer_text_never_count_as_yours");
    }
    #[test]
    fn test_this_session_lines_are_left_out() {
        check("test_this_session_lines_are_left_out");
    }
    #[test]
    fn test_a_line_older_than_30_days_is_marked_context_only_with_its_age() {
        check("test_a_line_older_than_30_days_is_marked_context_only_with_its_age");
    }
    #[test]
    fn test_your_no_after_sending_it_rejects_it_and_the_brief_quotes_your_words() {
        check("test_your_no_after_sending_it_rejects_it_and_the_brief_quotes_your_words");
    }
}
